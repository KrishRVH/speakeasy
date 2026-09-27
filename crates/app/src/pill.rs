use crate::runtime::{Phase, Snapshot};
use gpui::{prelude::*, *};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use raw_window_handle::HasWindowHandle;
use speakeasy_core::motion::Spring;
use std::time::{Duration, Instant};

const SUBMITTED_FOR: Duration = Duration::from_millis(350);
const EMPTY_FOR: Duration = Duration::from_millis(1200);
const ERROR_FOR: Duration = Duration::from_secs(8);
const LIMIT_SECONDS: u64 = 300;

// Deadlines are presentation-only. A wake only redraws the current snapshot;
// it never publishes session state or changes a capture/insertion deadline.
fn feedback_remaining(phase: Phase, elapsed: Duration) -> Option<Duration> {
    let duration = match phase {
        Phase::Done => SUBMITTED_FOR,
        Phase::Empty => EMPTY_FOR,
        Phase::Error => ERROR_FOR,
        _ => return None,
    };
    duration.checked_sub(elapsed).filter(|left| !left.is_zero())
}

fn recording_clock(seconds: u64) -> (bool, String) {
    if seconds >= LIMIT_SECONDS - 30 {
        (
            true,
            format!("0:{:02} left", LIMIT_SECONDS.saturating_sub(seconds)),
        )
    } else {
        (false, format!("{}:{:02}", seconds / 60, seconds % 60))
    }
}

pub struct Pill {
    snapshot: Snapshot,
    width: Spring,
    height: Spring,
    opacity: Spring,
    lift: Spring,
    waveform: Spring,
    meter: [Spring; 24],
    history: [f32; 24],
    frame_at: Instant,
    phase_at: Instant,
    hint_until: Option<Instant>,
    reduced: bool,
    visible: bool,
    animating: bool,
    wake: Option<Task<()>>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    visibility: Option<Task<()>>,
    _updates: Task<()>,
}

impl Pill {
    pub fn set_reduced(&mut self, reduced: bool) {
        self.reduced = reduced;
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub fn tray_hint(&mut self, cx: &mut Context<Self>) -> bool {
        if self.visible {
            return false;
        }
        self.hint_until = Some(Instant::now() + Duration::from_secs(3));
        self.wake = None;
        // Hidden native windows need showing before GPUI can render the hint.
        let handle = cx.global::<crate::shell::Services>().pill;
        self.visible = true;
        self.visibility = Some(set_visible(handle.into(), cx));
        cx.notify();
        true
    }

    pub fn new(
        mut updates: tokio::sync::watch::Receiver<Snapshot>,
        reduced: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let task = cx.spawn_in(window, async move |this, cx| {
            loop {
                let snapshot = updates.borrow_and_update().clone();
                if this
                    .update_in(cx, |pill, window, cx| {
                        let now = Instant::now();
                        let old_history = pill.history;
                        let state_changed = snapshot.phase != pill.snapshot.phase
                            || snapshot.id != pill.snapshot.id
                            || snapshot.hands_free != pill.snapshot.hands_free
                            || snapshot.message != pill.snapshot.message;
                        if snapshot.phase != pill.snapshot.phase
                            || snapshot.id != pill.snapshot.id
                            || (snapshot.phase == Phase::Error
                                && snapshot.message != pill.snapshot.message)
                        {
                            pill.phase_at = now;
                            pill.wake = None;
                            pill.hint_until = None;
                        }
                        if snapshot.id != pill.snapshot.id {
                            pill.history.fill(0.0);
                        }
                        if snapshot.phase == Phase::Recording
                            && snapshot.meter_tick != pill.snapshot.meter_tick
                        {
                            pill.history.rotate_left(1);
                            if let Some(last) = pill.history.last_mut() {
                                *last = snapshot.level;
                            }
                        }
                        if state_changed
                            && (matches!(
                                snapshot.phase,
                                Phase::Starting
                                    | Phase::Recording
                                    | Phase::Stopping
                                    | Phase::Processing
                            ) || feedback_remaining(
                                snapshot.phase,
                                now.duration_since(pill.phase_at),
                            )
                            .is_some())
                            && !pill.visible
                        {
                            pill.visible = true;
                            #[cfg(any(target_os = "windows", target_os = "macos"))]
                            {
                                pill.visibility =
                                    Some(set_visible(Window::window_handle(window), cx));
                            }
                            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
                            let _ = window;
                        }
                        pill.snapshot = snapshot;
                        if state_changed || old_history != pill.history {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                if updates.changed().await.is_err() {
                    break;
                }
            }
        });
        Self {
            snapshot: Snapshot::default(),
            width: Spring::new(56.0),
            height: Spring::new(28.0),
            opacity: Spring::new(0.0),
            lift: Spring::new(6.0),
            waveform: Spring::new(0.0),
            meter: std::array::from_fn(|_| Spring::new(0.0)),
            history: [0.0; 24],
            frame_at: Instant::now(),
            phase_at: Instant::now(),
            hint_until: None,
            reduced,
            visible: false,
            animating: false,
            wake: None,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            visibility: None,
            _updates: task,
        }
    }
}

impl Render for Pill {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let dt = if self.animating {
            now.duration_since(self.frame_at).as_secs_f32()
        } else {
            0.0
        };
        self.frame_at = now;
        let elapsed = now.duration_since(self.phase_at);
        let phase = self.snapshot.phase;
        let capturing = matches!(phase, Phase::Recording | Phase::Stopping);
        let remaining = feedback_remaining(phase, elapsed);
        let hint = self
            .hint_until
            .and_then(|until| until.checked_duration_since(now));
        let show = matches!(
            phase,
            Phase::Starting | Phase::Recording | Phase::Stopping | Phase::Processing
        ) || remaining.is_some()
            || hint.is_some();
        let (near_limit, clock) =
            recording_clock(now.duration_since(self.snapshot.started).as_secs());
        let show_clock = capturing && (self.snapshot.hands_free || near_limit);
        let (width, height) = if !show {
            (56.0, 28.0)
        } else if hint.is_some() {
            (360.0, 36.0)
        } else {
            match phase {
                Phase::Starting => (158.0, 32.0),
                Phase::Recording | Phase::Stopping => (
                    if near_limit {
                        248.0
                    } else if show_clock {
                        212.0
                    } else {
                        148.0
                    },
                    36.0,
                ),
                Phase::Processing => (88.0, 28.0),
                Phase::Done => (64.0, 28.0),
                Phase::Empty => (166.0, 32.0),
                Phase::Error => (360.0, 64.0),
                _ => (56.0, 28.0),
            }
        };
        self.width.target = width;
        self.height.target = height;
        self.opacity.target = if show { 1.0 } else { 0.0 };
        self.lift.target = if show { 0.0 } else { 6.0 };
        self.waveform.target = if capturing { 1.0 } else { 0.0 };
        let mut moving = false;
        for (spring, response) in [
            (&mut self.width, 28.0),
            (&mut self.height, 28.0),
            (&mut self.opacity, 40.0),
            (&mut self.lift, 32.0),
            (&mut self.waveform, 32.0),
        ] {
            if self.reduced {
                spring.snap();
            } else {
                spring.step_with_response(dt, response);
            }
            moving |= !spring.settled();
        }
        for (index, spring) in self.meter.iter_mut().enumerate() {
            spring.target = if phase == Phase::Recording {
                if self.reduced {
                    self.snapshot.level
                } else {
                    self.history[index]
                }
            } else {
                0.0
            };
            if self.reduced {
                spring.snap();
            } else {
                spring.step_with_response(
                    dt,
                    if spring.target > spring.value {
                        48.0
                    } else {
                        20.0
                    },
                );
            }
            moving |= !spring.settled();
        }
        let visible = show || self.opacity.value > 0.001;
        if !visible {
            // Native hidden windows stop receiving frames. Finish invisible
            // geometry now so the next entrance starts from rest, rather than
            // integrating the entire hidden interval as one animation frame.
            for spring in [
                &mut self.width,
                &mut self.height,
                &mut self.opacity,
                &mut self.lift,
                &mut self.waveform,
            ] {
                spring.snap();
            }
            for spring in &mut self.meter {
                spring.snap();
            }
            moving = false;
        }
        if visible != self.visible {
            self.visible = visible;
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            {
                self.visibility = Some(set_visible(Window::window_handle(window), cx));
            }
        }
        self.animating = visible && (moving || (phase == Phase::Processing && !self.reduced));
        if self.animating {
            window.request_animation_frame();
        }
        let wake_after = remaining.or(hint).or_else(|| {
            if capturing {
                Some(Duration::from_secs(1))
            } else if phase == Phase::Processing && elapsed < Duration::from_millis(250) {
                Some(Duration::from_millis(250) - elapsed)
            } else {
                None
            }
        });
        if self.wake.is_none()
            && let Some(delay) = wake_after
        {
            self.wake = Some(cx.spawn(async move |this, cx| {
                Timer::after(delay).await;
                let _ = this.update(cx, |pill, cx| {
                    pill.wake = None;
                    cx.notify();
                });
            }));
        }
        let content = if hint.is_some() {
            div()
                .text_size(px(12.0))
                .child(if cfg!(target_os = "macos") {
                    "In the menu bar · Use Speakeasy’s menu to quit"
                } else {
                    "In the tray · Use Speakeasy’s menu to quit"
                })
                .into_any_element()
        } else {
            match phase {
                Phase::Error => div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(symbol(Symbol::Attention, 0xf5a524))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_size(px(12.0))
                            .line_height(px(16.0))
                            .child(
                                div()
                                    .max_h(px(32.0))
                                    .overflow_hidden()
                                    .child(self.snapshot.message.clone()),
                            )
                            .child(div().text_color(rgb(0xa9aab2)).child("Details in Settings")),
                    )
                    .into_any_element(),
                Phase::Starting => div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(symbol(Symbol::Microphone, 0xa9aab2))
                    .child(div().text_size(px(12.0)).child("Opening mic…"))
                    .into_any_element(),
                Phase::Done => symbol(Symbol::Check, 0xe0e0e4).into_any_element(),
                Phase::Empty => div()
                    .text_size(px(12.0))
                    .child("No speech detected")
                    .into_any_element(),
                Phase::Processing => {
                    let progress =
                        ((elapsed.as_secs_f32() * std::f32::consts::TAU / 1.2).sin() + 1.0) / 2.0;
                    div()
                        .relative()
                        .w(px(60.0))
                        .h(px(22.0))
                        .child(
                            div()
                                .absolute()
                                .left(px(-17.0))
                                .opacity(self.waveform.value.clamp(0.0, 1.0))
                                .child(meter(
                                    self.meter.each_ref().map(|spring| spring.value),
                                    self.reduced,
                                )),
                        )
                        .child(
                            div()
                                .absolute()
                                .left(px(10.0))
                                .top(px(10.0))
                                .w(px(40.0))
                                .h(px(2.0))
                                .rounded_full()
                                .opacity((1.0 - self.waveform.value).clamp(0.0, 1.0))
                                .bg(rgb(0x505159))
                                .when(elapsed >= Duration::from_millis(250), |line| {
                                    line.child(
                                        div()
                                            .w(px(if self.reduced { 40.0 } else { 8.0 }))
                                            .h(px(2.0))
                                            .rounded_full()
                                            .ml(px(if self.reduced {
                                                0.0
                                            } else {
                                                progress * 32.0
                                            }))
                                            .bg(rgb(0xc1c2c8)),
                                    )
                                }),
                        )
                        .into_any_element()
                }
                Phase::Recording | Phase::Stopping => div()
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .child(div().size(px(8.0)).rounded_full().bg(rgb(0xff4f2e)))
                    .child(meter(
                        self.meter.each_ref().map(|spring| spring.value),
                        self.reduced,
                    ))
                    .when(show_clock, |row| {
                        row.when(self.snapshot.hands_free, |row| {
                            row.child(symbol(Symbol::Lock, 0xa9aab2))
                        })
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(if near_limit { 0xf5bd72 } else { 0xa9aab2 }))
                                .child(clock),
                        )
                    })
                    .into_any_element(),
                _ => div()
                    .w(px(18.0))
                    .h(px(2.0))
                    .rounded_full()
                    .bg(rgb(0x95969c))
                    .into_any_element(),
            }
        };
        div()
            .size_full()
            .flex()
            .items_end()
            .justify_center()
            .pb(px(20.0 - self.lift.value))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(14.0))
                    .w(px(self.width.value))
                    .h(px(self.height.value))
                    .rounded(px(self.height.value / 2.0))
                    .bg(rgba(0x17181bf5))
                    .border_1()
                    .border_color(rgba(0xffffff20))
                    .text_color(rgb(0xe7e7e9))
                    .shadow(vec![BoxShadow {
                        color: rgba(0x00000047).into(),
                        offset: point(px(0.0), px(4.0)),
                        blur_radius: px(16.0),
                        spread_radius: px(0.0),
                    }])
                    .overflow_hidden()
                    .opacity(self.opacity.value.clamp(0.0, 1.0))
                    .child(content),
            )
    }
}

#[derive(Clone, Copy)]
enum Symbol {
    Microphone,
    Lock,
    Check,
    Attention,
}

fn symbol(symbol: Symbol, color: u32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let mut path = PathBuilder::stroke(px(1.5));
            let mut line = |points: &[(f32, f32)]| {
                for (index, &(x, y)) in points.iter().enumerate() {
                    let point = bounds.origin + point(px(x), px(y));
                    if index == 0 {
                        path.move_to(point);
                    } else {
                        path.line_to(point);
                    }
                }
            };
            match symbol {
                Symbol::Check => line(&[(2.0, 8.0), (6.0, 12.0), (14.0, 4.0)]),
                Symbol::Attention => {
                    line(&[(8.0, 2.0), (8.0, 10.0)]);
                    line(&[(8.0, 12.0), (8.0, 14.0)]);
                }
                Symbol::Lock => {
                    line(&[(5.0, 7.0), (5.0, 3.0), (11.0, 3.0), (11.0, 7.0)]);
                    line(&[
                        (3.0, 7.0),
                        (13.0, 7.0),
                        (13.0, 14.0),
                        (3.0, 14.0),
                        (3.0, 7.0),
                    ]);
                }
                Symbol::Microphone => {
                    line(&[(6.0, 2.0), (10.0, 2.0), (10.0, 8.0), (6.0, 8.0), (6.0, 2.0)]);
                    line(&[(3.0, 6.0), (3.0, 11.0), (13.0, 11.0), (13.0, 6.0)]);
                    line(&[(8.0, 11.0), (8.0, 14.0)]);
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(color));
            }
        },
    )
    .size(px(16.0))
    .flex_shrink_0()
}

// The decorative bars share one layout element; their geometry stays independent
// of layout and hit testing as the audio level changes.
fn meter(levels: [f32; 24], reduced: bool) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let width = if reduced { 94.0 } else { 2.0 };
            for (index, level) in levels.iter().take(if reduced { 1 } else { 24 }).enumerate() {
                let height = 2.0 + level * 20.0;
                let origin =
                    bounds.origin + point(px(index as f32 * 4.0), px((22.0 - height) / 2.0));
                window.paint_quad(
                    fill(
                        Bounds::new(origin, size(px(width), px(height))),
                        rgb(0xe0e0e4),
                    )
                    .corner_radii(px(width.min(height) / 2.0)),
                );
            }
        },
    )
    .w(px(94.0))
    .h(px(22.0))
}

// Native show/resize can synchronously request a GPUI frame. Run it after
// releasing the app/window borrow, and resolve the handle just before use.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn set_visible(handle: AnyWindowHandle, cx: &mut App) -> Task<()> {
    cx.spawn(async move |cx| {
        let raw = cx.update_window(handle, |root, window, cx| {
            let visible = root
                .downcast::<Pill>()
                .ok()
                .map(|pill| pill.read(cx).visible);
            (
                HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()),
                visible,
            )
        });
        if let Ok((Ok(raw), Some(visible))) = raw {
            speakeasy_platform::set_pill_visible(raw, visible);
        }
    })
}

pub fn open(
    updates: tokio::sync::watch::Receiver<Snapshot>,
    reduced: bool,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Pill>> {
    let display = cx
        .primary_display()
        .ok_or_else(|| anyhow::anyhow!("No display available"))?;
    let screen = display.bounds();
    let bounds = Bounds::new(
        point(
            screen.origin.x + screen.size.width / 2.0 - px(200.0),
            screen.origin.y + screen.size.height - px(150.0),
        ),
        size(px(400.0), px(100.0)),
    );
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let mut native_result = Ok(());
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: false,
            show: cfg!(target_os = "linux"),
            kind: if cfg!(target_os = "linux") {
                WindowKind::Normal
            } else {
                WindowKind::PopUp
            },
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            window.set_window_title("Speakeasy pill");
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let _ = window;
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            {
                native_result = window
                    .window_handle()
                    .map_err(|error| anyhow::anyhow!("Cannot access pill window: {error}"))
                    .and_then(|handle| speakeasy_platform::configure_pill(handle.as_raw()));
            }
            cx.new(|cx| Pill::new(updates, reduced, window, cx))
        },
    )?;
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    native_result?;
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::{ERROR_FOR, SUBMITTED_FOR, feedback_remaining, recording_clock};
    use crate::runtime::Phase;
    use std::time::Duration;

    #[test]
    fn feedback_deadlines_expire_and_never_apply_to_recording_or_cancellation() {
        assert_eq!(
            feedback_remaining(Phase::Done, Duration::ZERO),
            Some(SUBMITTED_FOR)
        );
        assert_eq!(feedback_remaining(Phase::Done, SUBMITTED_FOR), None);
        assert_eq!(feedback_remaining(Phase::Recording, Duration::ZERO), None);
        assert_eq!(feedback_remaining(Phase::Recording, SUBMITTED_FOR), None);
        assert_eq!(feedback_remaining(Phase::Cancelled, Duration::ZERO), None);
        assert_eq!(feedback_remaining(Phase::Error, ERROR_FOR), None);
    }

    #[test]
    fn limit_countdown_is_clamped_and_only_appears_in_the_last_thirty_seconds() {
        assert_eq!(recording_clock(269), (false, "4:29".into()));
        assert_eq!(recording_clock(270), (true, "0:30 left".into()));
        assert_eq!(recording_clock(305), (true, "0:00 left".into()));
    }
}
