use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::BackgroundExecutor;
use speakeasy_core::gesture::RECORDING_LIMIT;
use speakeasy_platform::{Display, Part};
use speakeasy_show::{Mark, Moment, Point, Rect, Shape, Span};
use tokio::sync::watch;

use crate::{
    pill::COUNTDOWN,
    runtime::{Meter, ModelState, Phase, Snapshot},
    show::Canvas,
};

const NEAR_RECORDING_LIMIT: Duration = RECORDING_LIMIT
    .saturating_sub(COUNTDOWN)
    .saturating_add(Duration::from_secs(2));

pub(super) struct Script {
    clock: BackgroundExecutor,
    output: watch::Sender<Snapshot>,
    meter: watch::Sender<Meter>,
    canvas: watch::Sender<Canvas>,
}

impl Script {
    pub(super) fn new(
        clock: BackgroundExecutor,
        output: watch::Sender<Snapshot>,
        meter: watch::Sender<Meter>,
        canvas: watch::Sender<Canvas>,
    ) -> Self {
        Self {
            clock,
            output,
            meter,
            canvas,
        }
    }

    pub(super) async fn play(self) {
        let mut snapshot = starting(self.output.borrow().session.wrapping_add(1));
        self.present(&snapshot, Duration::from_millis(350)).await;
        snapshot.phase = Phase::Recording;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            let seconds = start.elapsed().as_secs_f32();
            snapshot.hands_free = seconds > 2.0;
            let level =
                (0.35 + 0.45 * (seconds * 7.0).sin()).max(0.0) * (seconds * 2.1).sin().abs();
            self.measure(&snapshot, level);
            self.present(&snapshot, Duration::from_millis(32)).await;
        }
        snapshot.started = Instant::now()
            .checked_sub(NEAR_RECORDING_LIMIT)
            .unwrap_or_else(Instant::now);
        self.measure(&snapshot, 0.0);
        self.present(&snapshot, Duration::from_secs(2)).await;
        for (phase, duration) in [
            (Phase::Stopping, Duration::from_millis(150)),
            (Phase::Processing, Duration::from_millis(1400)),
            (Phase::Done, Duration::from_millis(450)),
        ] {
            snapshot.phase = phase;
            self.present(&snapshot, duration).await;
        }
        let show_session = snapshot.session.wrapping_add(1);
        self.show(show_session).await;
        self.interruptions(show_session).await;
    }

    async fn show(&self, session: u64) {
        let display = Display::under_pointer().ok();
        let clear = Clear::new(&self.canvas);
        let mut snapshot = starting(session);
        self.present(&snapshot, Duration::from_millis(250)).await;
        snapshot.phase = Phase::Recording;
        let start = Instant::now();
        let mut lifted = false;

        while start.elapsed() < Duration::from_millis(4500) {
            let seconds = start.elapsed().as_secs_f32();
            let show = seconds > 0.8;
            if show && let Some(display) = display {
                if !snapshot.show {
                    self.canvas
                        .send_replace(Canvas::covering(display, Part::WHOLE));
                }
                if !lifted {
                    lifted = circle(&self.canvas, display, seconds);
                }
            }
            snapshot.show = show;
            snapshot.hands_free = show;
            let level = (0.3 + 0.3 * (seconds * 5.0).sin()).max(0.0);
            self.measure(&snapshot, level);
            self.present(&snapshot, Duration::from_millis(32)).await;
        }
        drop(clear);
        snapshot.hands_free = false;
        for (phase, duration) in [
            (Phase::Stopping, Duration::from_millis(150)),
            (Phase::Processing, Duration::from_millis(1200)),
            (Phase::Copied, Duration::from_millis(1600)),
        ] {
            snapshot.phase = phase;
            self.present(&snapshot, duration).await;
        }
    }

    async fn interruptions(&self, mut session: u64) {
        for (ending, message, held) in [
            (Phase::Cancelled, "", Duration::from_millis(60)),
            (Phase::Empty, "", Duration::from_secs(2)),
            (
                Phase::Copied,
                "Text copied. Release your shortcut keys and paste into your editor.",
                Duration::from_secs(2),
            ),
            (
                Phase::Error,
                "The selected microphone is disconnected. Reconnect it, or choose another microphone in Settings.",
                Duration::from_secs(2),
            ),
        ] {
            session = session.wrapping_add(1);
            let mut snapshot = starting(session);
            self.present(&snapshot, Duration::from_millis(180)).await;
            snapshot.phase = Phase::Recording;
            self.present(&snapshot, Duration::from_millis(700)).await;
            snapshot.phase = ending;
            snapshot.message = message.into();
            self.present(&snapshot, held).await;
        }
    }

    async fn present(&self, snapshot: &Snapshot, duration: Duration) {
        self.output.send_if_modified(|current| {
            let changed = *current != *snapshot;
            if changed {
                current.clone_from(snapshot);
            }
            changed
        });
        self.clock.timer(duration).await;
    }

    fn measure(&self, snapshot: &Snapshot, level: f32) {
        self.meter.send_replace(Meter {
            epoch: snapshot.epoch,
            session: snapshot.session,
            level,
        });
    }
}

struct Clear<'a>(&'a watch::Sender<Canvas>);

impl<'a> Clear<'a> {
    fn new(canvas: &'a watch::Sender<Canvas>) -> Self {
        canvas.send_replace(Canvas::default());
        Self(canvas)
    }
}

impl Drop for Clear<'_> {
    fn drop(&mut self) {
        self.0.send_replace(Canvas::default());
    }
}

fn circle(canvas: &watch::Sender<Canvas>, display: Display, seconds: f32) -> bool {
    const DRAWN_FROM: f32 = 1.0;
    const LIFTED_AT: f32 = 1.8;
    const CENTER: Point = Point::new(0.5, 0.45);
    const RADIUS: f32 = 0.1;

    const TURNS: f32 = 1.08;
    let progress = (seconds - DRAWN_FROM) / (LIFTED_AT - DRAWN_FROM);
    let [width, height] = display.size;
    let across = RADIUS * height / width;

    let points: Vec<Point> = (0..=100_u16)
        .map(|step| f32::from(step) / 100.0)
        .take_while(|&share| share <= progress)
        .map(|share| {
            let turn = share * std::f32::consts::TAU * TURNS;
            Point::new(
                CENTER.x + across * turn.cos(),
                CENTER.y + RADIUS * turn.sin(),
            )
        })
        .collect();
    let lifted = progress >= 1.0;
    let now = Instant::now();
    canvas.send_if_modified(|canvas| {
        let Some(sheet) = canvas.sheet_mut() else {
            return false;
        };
        if lifted {
            let mark = Mark {
                number: 1,
                shape: Shape::Circle,
                strokes: 0..1,
                span: Span::at(Moment::START),
                bounds: Rect {
                    left: CENTER.x - across,
                    top: CENTER.y - RADIUS,
                    right: CENTER.x + across,
                    bottom: CENTER.y + RADIUS,
                },
            };
            sheet.update([Arc::from(points)], [], Some(Arc::new([mark])), now)
        } else {
            sheet.update([], points, None, now)
        }
    });
    lifted
}

fn starting(session: u64) -> Snapshot {
    Snapshot {
        session,
        phase: Phase::Starting,
        model: ModelState::Ready,
        ..Snapshot::default()
    }
}
