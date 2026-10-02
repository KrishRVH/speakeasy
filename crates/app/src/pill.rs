//! The floating pill: a borderless popup that animates the latest session snapshot. It only
//! presents; wakes and frames redraw the current snapshot and never publish session state or move a
//! capture or insertion deadline.

mod grille;
mod symbol;

use std::time::{Duration, Instant};

use anyhow::Context as _;
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext, Bounds, BoxShadow, Div, Size, Task, Timer,
    Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
    canvas, div, point, prelude::*, px, rgb, rgba, size,
};
use speakeasy_core::{gesture::RECORDING_LIMIT, motion::Spring};
use speakeasy_dictation::{
    runtime::{Phase, Snapshot},
    theme::{Palette, Theme},
};
use speakeasy_platform::{PILL_HEIGHT, PILL_WIDTH};
use tokio::sync::watch;

use self::{grille::Grille, symbol::Symbol};
use crate::gpui_ext::{EntityUpdate, alpha, raw_handle};

const METER_BARS: usize = 24;
const FRAME_INTERVAL: Duration = Duration::from_millis(5);

const SUBMITTED_FOR: Duration = Duration::from_millis(350);
const EMPTY_FOR: Duration = Duration::from_millis(1200);
const ERROR_FOR: Duration = Duration::from_secs(8);
const TRAY_HINT_FOR: Duration = Duration::from_secs(3);

const CLOCK_TICK: Duration = Duration::from_secs(1);
/// The final stretch of a recording during which the pill counts down to the limit.
pub(crate) const COUNTDOWN: Duration = Duration::from_secs(30);
const SWEEP_DELAY: Duration = Duration::from_millis(250);
const SWEEP_PERIOD: Duration = Duration::from_millis(1200);

const RESTING_SIZE: Size<f32> = size(60.0, 30.0);
const RECORDING_WIDTH: f32 = 152.0;
const MESSAGE_WIDTH: f32 = 360.0;
const HIDDEN_LIFT: f32 = 6.0;
const SHADOW_OFFSET: f32 = 4.0;
const SHADOW_BLUR: f32 = 16.0;
/// Room under the shown capsule for its drop shadow and for the `HIDDEN_LIFT` it sinks while hiding.
const SHADOW_ROOM: f32 = SHADOW_OFFSET + SHADOW_BLUR + HIDDEN_LIFT;
/// The window's clearance above the display's bottom edge. Windows and macOS move the pill onto
/// their work area; GPUI's X11 display has none, so on Linux this keeps it above a bottom panel.
const BOTTOM_CLEARANCE: f32 = 50.0;

const LID_OPEN: f32 = 0.0;
const LID_AJAR: f32 = 0.62;
const LID_CLOSED: f32 = 1.0;

const TRAY_HINT: &str = if cfg!(target_os = "macos") {
    "In the menu bar · Use Speakeasy’s menu to quit"
} else {
    "In the tray · Use Speakeasy’s menu to quit"
};

#[expect(
    clippy::struct_excessive_bools,
    reason = "Reduced motion, native visibility, animation and frame registration are independent presentation facts; none owns session state"
)]
pub(crate) struct Pill {
    snapshot: Snapshot,
    capsule: Capsule,
    meter: [Spring; METER_BARS],
    history: [f32; METER_BARS],
    window: AnyWindowHandle,
    phase_at: Instant,
    hint_until: Option<Instant>,
    frame_at: Instant,
    frame_due: Instant,
    reduced_motion: bool,
    theme: Theme,
    visible: bool,
    animating: bool,
    frame_pending: bool,
    wake: Option<Task<()>>,
    visibility: Option<Task<()>>,
    _updates: Task<()>,
}

impl Pill {
    fn new(
        updates: watch::Receiver<Snapshot>,
        reduced_motion: bool,
        theme: Theme,
        window: &Window,
        cx: &Context<Self>,
    ) -> Self {
        let now = Instant::now();
        Self {
            snapshot: Snapshot::default(),
            capsule: Capsule::hidden(),
            meter: std::array::from_fn(|_| Spring::new(0.0)),
            history: [0.0; METER_BARS],
            window: window.window_handle(),
            phase_at: now,
            hint_until: None,
            frame_at: now,
            frame_due: now,
            reduced_motion,
            theme,
            visible: false,
            animating: false,
            frame_pending: false,
            wake: None,
            visibility: None,
            _updates: Self::follow(updates, window, cx),
        }
    }

    /// Applies the current snapshot, then each later one, while the window and publisher live.
    fn follow(
        mut updates: watch::Receiver<Snapshot>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Task<()> {
        updates.mark_changed();
        cx.spawn_in(window, async move |this, cx| {
            while updates.changed().await.is_ok() {
                let snapshot = updates.borrow_and_update().clone();
                if this
                    .update_in(cx, |pill, _, cx| pill.apply_snapshot(snapshot, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    pub(crate) fn set_reduced_motion(&mut self, reduced_motion: bool) {
        self.reduced_motion = reduced_motion;
    }

    pub(crate) fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Shows the tray hint unless the pill is already visible; returns whether it did.
    pub(crate) fn tray_hint(&mut self, cx: &mut Context<Self>) -> bool {
        if self.visible {
            return false;
        }
        self.hint_until = Instant::now().checked_add(TRAY_HINT_FOR);
        self.wake = None;
        self.set_visible(true, cx);
        cx.notify();
        true
    }

    fn apply_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        let now = Instant::now();
        let previous = &self.snapshot;
        let new_session = snapshot.id != previous.id;
        let new_phase = new_session || snapshot.phase != previous.phase;
        let state_changed = new_phase
            || snapshot.hands_free != previous.hands_free
            || snapshot.message != previous.message;
        if new_phase || (snapshot.phase == Phase::Error && snapshot.message != previous.message) {
            self.phase_at = now;
            self.wake = None;
            self.hint_until = None;
        }
        let previous_history = self.history;
        if new_session {
            self.history.fill(0.0);
        }
        if snapshot.phase == Phase::Recording && snapshot.meter_tick != previous.meter_tick {
            self.history.rotate_left(1);
            if let Some(newest) = self.history.last_mut() {
                *newest = snapshot.level;
            }
        }
        // Exact comparison: any tolerance would hide quiet speech.
        let history_changed = previous_history.map(f32::to_bits) != self.history.map(f32::to_bits);
        // A hidden native window never renders, so it cannot wait for `render` to show it.
        if state_changed
            && !self.visible
            && (snapshot.phase.is_active()
                || feedback_remaining(snapshot.phase, now.duration_since(self.phase_at)).is_some())
        {
            self.set_visible(true, cx);
        }
        self.snapshot = snapshot;
        // An animating pill paints new levels on its pending frame.
        if state_changed || (history_changed && !self.animating) {
            cx.notify();
        }
    }

    fn scene(&self, now: Instant) -> Scene {
        let phase = self.snapshot.phase;
        let since_phase = now.duration_since(self.phase_at);
        Scene {
            phase,
            since_phase,
            feedback: feedback_remaining(phase, since_phase),
            hint: self
                .hint_until
                .and_then(|until| until.checked_duration_since(now)),
            hands_free: self.snapshot.hands_free,
            clock: Clock::new(now.duration_since(self.snapshot.started).as_secs()),
        }
    }

    /// Retargets every spring for `scene`, steps it by `since_frame`, and returns whether any is
    /// still moving.
    fn advance_springs(&mut self, scene: &Scene, since_frame: Duration) -> bool {
        self.capsule.retarget(scene);
        let reduced_motion = self.reduced_motion;
        let advance = |spring: &mut Spring, omega: f32| {
            if reduced_motion {
                spring.snap();
            } else {
                spring.step(since_frame, omega);
            }
            !spring.settled()
        };
        let mut moving = false;
        for (spring, omega) in self.capsule.springs_mut() {
            moving |= advance(spring, omega);
        }
        for (spring, level) in self.meter.iter_mut().zip(self.history) {
            spring.target = match scene.phase {
                Phase::Recording if reduced_motion => self.snapshot.level,
                Phase::Recording => level,
                _ => 0.0,
            };
            // Bars rise fast enough to catch a syllable and fall slowly enough to read.
            let omega = if spring.target > spring.value() {
                48.0
            } else {
                20.0
            };
            moving |= advance(spring, omega);
        }
        moving
    }

    /// Hidden native windows receive no frames, so springs settle now; the next entrance then
    /// starts at rest instead of replaying the hidden interval as one frame.
    fn rest_while_hidden(&mut self) {
        for (spring, _) in self.capsule.springs_mut() {
            spring.snap();
        }
        for spring in &mut self.meter {
            spring.snap();
        }
    }

    fn set_visible(&mut self, visible: bool, cx: &App) {
        self.visible = visible;
        self.visibility = Some(apply_native_visibility(self.window, visible, cx));
    }

    /// Redraws on the first native frame at or after `frame_due`, staying in display sync without a
    /// repeating timer or redraws between deadlines.
    fn request_deadline_frame(&mut self, window: &Window, cx: &Context<Self>) {
        if self.frame_pending {
            return;
        }
        self.frame_pending = true;
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            this.update_if_alive(cx, |pill, cx| {
                pill.frame_pending = false;
                if !pill.animating {
                    return;
                }
                if Instant::now() >= pill.frame_due {
                    cx.notify();
                } else {
                    pill.request_deadline_frame(window, cx);
                }
            });
        });
    }

    fn schedule_wake(&mut self, delay: Option<Duration>, cx: &Context<Self>) {
        if self.wake.is_none()
            && let Some(delay) = delay
        {
            self.wake = Some(cx.spawn(async move |this, cx| {
                Timer::after(delay).await;
                this.update_if_alive(cx, |pill, cx| {
                    pill.wake = None;
                    cx.notify();
                });
            }));
        }
    }

    fn render_capsule(&self, scene: Scene) -> Div {
        let palette = self.theme.palette();
        let height = self.capsule.height.value();
        let inlaid = scene.has_slot() && height > 22.0;
        div()
            .size_full()
            .flex()
            .items_end()
            .justify_center()
            .pb(px(SHADOW_ROOM - self.capsule.lift.value()))
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(self.capsule.width.value()))
                    .h(px(height))
                    .rounded(px(height / 2.0))
                    .bg(alpha(palette.door, 0.96))
                    .border_1()
                    .border_color(alpha(palette.ink, 0.13))
                    .text_color(rgb(palette.ink))
                    .shadow(vec![BoxShadow {
                        color: rgba(0x0000_0052).into(),
                        offset: point(px(0.0), px(SHADOW_OFFSET)),
                        blur_radius: px(SHADOW_BLUR),
                        spread_radius: px(0.0),
                    }])
                    .overflow_hidden()
                    .opacity(self.capsule.opacity.value().clamp(0.0, 1.0))
                    .when(inlaid, |capsule| {
                        capsule.child(brass_inlay(height, palette))
                    })
                    .child(self.render_content(scene, palette)),
            )
    }

    fn render_content(&self, scene: Scene, palette: &'static Palette) -> AnyElement {
        if scene.hint.is_some() {
            div()
                .text_size(px(12.0))
                .child(TRAY_HINT)
                .into_any_element()
        } else if scene.phase == Phase::Error {
            self.render_error(palette).into_any_element()
        } else if scene.has_slot() {
            self.render_slot(scene, palette).into_any_element()
        } else {
            idle_mark(palette).into_any_element()
        }
    }

    fn render_error(&self, palette: &Palette) -> Div {
        div()
            .w_full()
            .px(px(14.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(Symbol::Attention.element(palette.warn))
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
                    .child(
                        div()
                            .text_color(rgb(palette.muted))
                            .child("Details in Settings"),
                    ),
            )
    }

    fn render_slot(&self, scene: Scene, palette: &'static Palette) -> Div {
        let (width, height) = (self.capsule.width.value(), self.capsule.height.value());
        let shows_clock = scene.shows_clock();
        let clock_width = if shows_clock {
            (width - RECORDING_WIDTH).max(0.0)
        } else {
            0.0
        };
        let slot_height = (height - 16.0).max(4.0);
        let grille = Grille {
            lid: self.capsule.lid.value().clamp(0.0, 1.0),
            waveform: self.capsule.waveform.value().clamp(0.0, 1.0),
            levels: self.meter.each_ref().map(Spring::value),
            reduced_motion: self.reduced_motion,
            sweep: scene.sweep(self.reduced_motion),
            palette,
        };
        div()
            .w_full()
            .px(px(7.0))
            .flex()
            .items_center()
            .gap(px(11.0))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w(px((width - 16.0 - clock_width).max(6.0)))
                    .h(px(slot_height))
                    .rounded(px(slot_height / 2.0))
                    .bg(alpha(0x00_00_00, 0.5))
                    .border_1()
                    .border_color(if grille.shows_waveform() {
                        alpha(palette.live, 0.45 * grille.waveform)
                    } else {
                        alpha(palette.ink, 0.08)
                    })
                    .child(
                        canvas(
                            |_, _, _| (),
                            move |bounds, (), window, _| grille.paint(bounds, window),
                        )
                        .size_full(),
                    )
                    .when_some(slot_label(scene.phase, palette), |slot, label| {
                        slot.child(
                            div()
                                .absolute()
                                .top(px(0.0))
                                .left(px(0.0))
                                .size_full()
                                .pl(px(10.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .opacity(((grille.lid - 0.6) / 0.4).clamp(0.0, 1.0))
                                .child(label),
                        )
                    }),
            )
            .when(shows_clock, |row| {
                row.when(scene.hands_free, |row| {
                    row.child(Symbol::Lock.element(palette.muted))
                })
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(if scene.clock.near_limit {
                            palette.warn
                        } else {
                            palette.muted
                        }))
                        .child(scene.clock.text),
                )
            })
    }
}

impl Render for Pill {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let since_frame = if self.animating {
            now.duration_since(self.frame_at)
        } else {
            Duration::ZERO
        };
        self.frame_at = now;
        let scene = self.scene(now);
        let moving = self.advance_springs(&scene, since_frame);
        let visible = scene.is_shown() || self.capsule.opacity.value() > 0.001;
        if !visible {
            self.rest_while_hidden();
        }
        if visible != self.visible {
            self.set_visible(visible, cx);
        }
        self.animating =
            visible && (moving || (scene.phase == Phase::Processing && !self.reduced_motion));
        self.frame_due = next_frame_deadline(self.frame_due, now);
        if self.animating {
            self.request_deadline_frame(window, cx);
        }
        self.schedule_wake(scene.wake_after(), cx);
        self.render_capsule(scene)
    }
}

/// The capsule's geometry and its slot's lid and waveform, each eased by a spring.
struct Capsule {
    width: Spring,
    height: Spring,
    opacity: Spring,
    lift: Spring,
    waveform: Spring,
    lid: Spring,
}

impl Capsule {
    fn hidden() -> Self {
        Self {
            width: Spring::new(RESTING_SIZE.width),
            height: Spring::new(RESTING_SIZE.height),
            opacity: Spring::new(0.0),
            lift: Spring::new(HIDDEN_LIFT),
            waveform: Spring::new(0.0),
            lid: Spring::new(LID_CLOSED),
        }
    }

    fn retarget(&mut self, scene: &Scene) {
        let shown = scene.is_shown();
        let size = scene.capsule_size();
        self.width.target = size.width;
        self.height.target = size.height;
        self.opacity.target = if shown { 1.0 } else { 0.0 };
        self.lift.target = if shown { 0.0 } else { HIDDEN_LIFT };
        self.waveform.target = if scene.is_capturing() { 1.0 } else { 0.0 };
        self.lid.target = lid_target(scene.phase);
    }

    fn springs_mut(&mut self) -> [(&mut Spring, f32); 6] {
        [
            (&mut self.width, 28.0),
            (&mut self.height, 28.0),
            (&mut self.opacity, 40.0),
            (&mut self.lift, 32.0),
            (&mut self.waveform, 32.0),
            (&mut self.lid, 24.0),
        ]
    }
}

/// What the pill presents at one instant.
struct Scene {
    phase: Phase,
    since_phase: Duration,
    feedback: Option<Duration>,
    hint: Option<Duration>,
    hands_free: bool,
    clock: Clock,
}

impl Scene {
    fn is_shown(&self) -> bool {
        self.phase.is_active() || self.feedback.is_some() || self.hint.is_some()
    }

    fn is_capturing(&self) -> bool {
        matches!(self.phase, Phase::Recording | Phase::Stopping)
    }

    fn has_slot(&self) -> bool {
        self.hint.is_none()
            && matches!(
                self.phase,
                Phase::Starting
                    | Phase::Recording
                    | Phase::Stopping
                    | Phase::Processing
                    | Phase::Done
                    | Phase::Empty
            )
    }

    fn shows_clock(&self) -> bool {
        self.is_capturing() && (self.hands_free || self.clock.near_limit)
    }

    fn capsule_size(&self) -> Size<f32> {
        if !self.is_shown() {
            return RESTING_SIZE;
        }
        if self.hint.is_some() {
            return size(MESSAGE_WIDTH, 36.0);
        }
        match self.phase {
            Phase::Starting => size(172.0, 38.0),
            Phase::Recording | Phase::Stopping if self.clock.near_limit => size(262.0, 38.0),
            Phase::Recording | Phase::Stopping if self.shows_clock() => size(228.0, 38.0),
            Phase::Recording | Phase::Stopping => size(RECORDING_WIDTH, 38.0),
            Phase::Processing => size(104.0, 32.0),
            Phase::Done => size(76.0, 34.0),
            Phase::Empty => size(182.0, 36.0),
            Phase::Error => size(MESSAGE_WIDTH, 64.0),
            Phase::Idle | Phase::Cancelled => RESTING_SIZE,
        }
    }

    fn wake_after(&self) -> Option<Duration> {
        self.feedback.or(self.hint).or_else(|| {
            if self.is_capturing() {
                Some(CLOCK_TICK)
            } else if self.phase == Phase::Processing && self.since_phase < SWEEP_DELAY {
                Some(SWEEP_DELAY.saturating_sub(self.since_phase))
            } else {
                None
            }
        })
    }

    fn sweep(&self, reduced_motion: bool) -> Option<f32> {
        (self.phase == Phase::Processing && self.since_phase >= SWEEP_DELAY).then(|| {
            if reduced_motion {
                0.5
            } else {
                let turns = self.since_phase.div_duration_f32(SWEEP_PERIOD);
                f32::midpoint((turns * std::f32::consts::TAU).sin(), 1.0)
            }
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Clock {
    near_limit: bool,
    text: String,
}

impl Clock {
    fn new(seconds: u64) -> Self {
        let limit = RECORDING_LIMIT.as_secs();
        if seconds >= limit.saturating_sub(COUNTDOWN.as_secs()) {
            Self {
                near_limit: true,
                text: {
                    let left = limit.saturating_sub(seconds);
                    format!("{}:{:02} left", left / 60, left % 60)
                },
            }
        } else {
            Self {
                near_limit: false,
                text: format!("{}:{:02}", seconds / 60, seconds % 60),
            }
        }
    }
}

pub(crate) fn open(
    updates: watch::Receiver<Snapshot>,
    reduced_motion: bool,
    theme: Theme,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Pill>> {
    let display = cx.primary_display().context("No display available")?;
    let screen = display.bounds();
    let pill_size = size(px(f32::from(PILL_WIDTH)), px(f32::from(PILL_HEIGHT)));
    let bounds = Bounds::new(
        point(
            screen.origin.x + (screen.size.width - pill_size.width) / 2.0,
            screen.bottom() - pill_size.height - px(BOTTOM_CLEARANCE),
        ),
        pill_size,
    );
    let mut configured = Ok(());
    let pill = cx.open_window(
        WindowOptions {
            #[cfg(target_os = "linux")]
            app_id: Some(speakeasy_platform::APPLICATION_ID.into()),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            window.set_window_title("Speakeasy pill");
            configured = raw_handle(window, "pill").and_then(speakeasy_platform::configure_pill);
            cx.new(|cx| Pill::new(updates, reduced_motion, theme, window, cx))
        },
    )?;
    configured?;
    Ok(pill)
}

/// Applies `visible` in a task: native show can synchronously request a GPUI frame, so it must run
/// after the current window borrow ends. Replacing the task drops, and so cancels, an older one.
fn apply_native_visibility(pill: AnyWindowHandle, visible: bool, cx: &App) -> Task<()> {
    cx.spawn(async move |cx| {
        if let Ok(Ok(raw)) = cx.update_window(pill, |_, window, _| raw_handle(window, "pill")) {
            speakeasy_platform::set_pill_visible(raw, visible);
        }
    })
}

fn feedback_remaining(phase: Phase, since_phase: Duration) -> Option<Duration> {
    let duration = match phase {
        Phase::Done => SUBMITTED_FOR,
        Phase::Empty => EMPTY_FOR,
        Phase::Error => ERROR_FOR,
        _ => return None,
    };
    duration
        .checked_sub(since_phase)
        .filter(|left| !left.is_zero())
}

fn next_frame_deadline(previous: Instant, now: Instant) -> Instant {
    previous
        .checked_add(FRAME_INTERVAL)
        .filter(|next| *next > now)
        .or_else(|| now.checked_add(FRAME_INTERVAL))
        .unwrap_or(now)
}

fn lid_target(phase: Phase) -> f32 {
    match phase {
        Phase::Recording | Phase::Stopping => LID_OPEN,
        Phase::Processing => LID_AJAR,
        _ => LID_CLOSED,
    }
}

fn slot_label(phase: Phase, palette: &Palette) -> Option<AnyElement> {
    match phase {
        Phase::Starting => Some(
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(Symbol::Microphone.element(palette.muted))
                .child(div().text_size(px(12.0)).child("Opening mic…"))
                .into_any_element(),
        ),
        Phase::Empty => Some(
            div()
                .text_size(px(12.0))
                .child("No speech detected")
                .into_any_element(),
        ),
        Phase::Done => Some(Symbol::Check.element(palette.lamp).into_any_element()),
        _ => None,
    }
}

fn idle_mark(palette: &Palette) -> Div {
    div()
        .w(px(18.0))
        .h(px(2.0))
        .rounded_full()
        .bg(alpha(palette.ink, 0.5))
}

fn brass_inlay(height: f32, palette: &Palette) -> Div {
    div()
        .absolute()
        .top(px(2.5))
        .left(px(2.5))
        .right(px(2.5))
        .bottom(px(2.5))
        .rounded(px(height / 2.0 - 3.5))
        .border_1()
        .border_color(alpha(palette.lamp, 0.16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_frames_skip_missed_deadlines_and_on_time_frames_keep_cadence() {
        let deadline = Instant::now();
        let slightly_late = deadline + Duration::from_millis(1);
        assert_eq!(
            next_frame_deadline(deadline, slightly_late),
            deadline + FRAME_INTERVAL
        );
        for delay in [FRAME_INTERVAL, Duration::from_secs(60)] {
            let late = deadline + delay;
            assert_eq!(next_frame_deadline(deadline, late), late + FRAME_INTERVAL);
        }
        let at_60_hz = deadline + Duration::from_nanos(1_000_000_000 / 60);
        assert!(
            next_frame_deadline(deadline, deadline) < at_60_hz,
            "Slower displays must remain eligible on every native frame"
        );
    }

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
        let clock = |near_limit, text: &str| Clock {
            near_limit,
            text: text.into(),
        };
        assert_eq!(Clock::new(269), clock(false, "4:29"));
        assert_eq!(Clock::new(270), clock(true, "0:30 left"));
        assert_eq!(Clock::new(305), clock(true, "0:00 left"));
    }

    #[cfg(target_os = "linux")]
    mod native {
        use std::collections::HashSet;

        use anyhow::{anyhow, bail, ensure};
        use gpui::AsyncApp;
        use raw_window_handle::{
            HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
        };
        use x11rb::protocol::{
            shape::{self, ConnectionExt as _},
            xproto::{AtomEnum, ConnectionExt as _, ImageFormat, MapState},
        };

        use super::*;
        use crate::gpui_ext::AppUpdate;

        #[test]
        #[ignore = "Opt-in native windows on a private Xvfb display, never microphone, input, or clipboard"]
        fn idle_native_pill_starts_unmapped_and_can_show_and_hide() -> anyhow::Result<()> {
            run_native(async |cx: &AsyncApp| cx.update(check_idle_pill).flatten())
        }

        #[test]
        #[ignore = "Opt-in native windows on a private Xvfb display, never microphone, input, or clipboard"]
        fn idle_native_pill_can_render_a_window_opened_after_launch() -> anyhow::Result<()> {
            run_native(check_late_window)
        }

        fn run_native(
            check: impl AsyncFnOnce(&AsyncApp) -> anyhow::Result<()> + 'static,
        ) -> anyhow::Result<()> {
            let (completed, outcome) = std::sync::mpsc::channel();
            gpui::Application::new().run(move |cx| {
                cx.spawn(async move |cx| {
                    completed.send(check(cx).await).unwrap();
                    cx.update_if_running(|cx| cx.quit());
                })
                .detach();
            });
            outcome
                .try_recv()
                .context("Native check did not complete")?
        }

        fn check_idle_pill(cx: &mut App) -> anyhow::Result<()> {
            let (_publisher, updates) = watch::channel(Snapshot::default());
            let pill = open(updates, true, Theme::default(), cx)?;
            let (raw, display) = pill.update(cx, |_, window, _| {
                Ok::<_, anyhow::Error>((
                    HasWindowHandle::window_handle(window)
                        .map_err(|error| anyhow!("Window handle: {error}"))?
                        .as_raw(),
                    HasDisplayHandle::display_handle(window)
                        .map_err(|error| anyhow!("Display handle: {error}"))?
                        .as_raw(),
                ))
            })??;
            let id = match (raw, display) {
                (RawWindowHandle::Xcb(window), RawDisplayHandle::Xcb(display)) => {
                    ensure!(
                        display.connection.is_some(),
                        "Missing live XCB display handle"
                    );
                    window.window.get()
                },
                (RawWindowHandle::Xlib(window), RawDisplayHandle::Xlib(display)) => {
                    ensure!(
                        display.display.is_some(),
                        "Missing live Xlib display handle"
                    );
                    u32::try_from(window.window)?
                },
                _ => bail!("Expected matching X11 window and display handles"),
            };
            let (connection, _) = x11rb::connect(None)?;
            let attributes = connection.get_window_attributes(id)?.reply()?;
            ensure!(
                attributes.map_state == MapState::UNMAPPED,
                "Idle pill is natively mapped after open and its initial draw"
            );
            ensure!(
                attributes.override_redirect,
                "Pill is managed as an ordinary window"
            );
            let hints = connection
                .get_property(false, id, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, 0, 9)?
                .reply()?;
            let hints: Vec<_> = hints
                .value32()
                .context("Invalid WM_HINTS format")?
                .collect();
            ensure!(
                hints.len() == 9 && hints[0] & 1 != 0 && hints[1] == 0,
                "Pill accepts input focus"
            );
            ensure!(
                connection
                    .shape_get_rectangles(id, shape::SK::INPUT)?
                    .reply()?
                    .rectangles
                    .is_empty(),
                "Pill input region intercepts the pointer"
            );

            speakeasy_platform::set_pill_visible(raw, true);
            ensure!(
                connection.get_window_attributes(id)?.reply()?.map_state == MapState::VIEWABLE,
                "Pill did not become viewable"
            );
            speakeasy_platform::set_pill_visible(raw, false);
            ensure!(
                connection.get_window_attributes(id)?.reply()?.map_state == MapState::UNMAPPED,
                "Pill did not become unmapped"
            );
            Ok(())
        }

        struct LateColors;

        impl Render for LateColors {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .size_full()
                    .bg(rgb(0x11_33_55))
                    .child(div().w(px(32.0)).h_full().bg(rgb(0x55_77_99)))
            }
        }

        #[expect(
            clippy::future_not_send,
            reason = "Native acceptance creates and observes GPUI windows on their owning UI thread"
        )]
        async fn check_late_window(cx: &AsyncApp) -> anyhow::Result<()> {
            // Opening during launch would let startup event draining present the window.
            Timer::after(Duration::from_millis(100)).await;
            let id = cx.update(|cx| {
                let (_publisher, updates) = watch::channel(Snapshot::default());
                let _pill = open(updates, true, Theme::default(), cx)?;
                let colors = cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.0), px(0.0)),
                            size(px(64.0), px(64.0)),
                        ))),
                        titlebar: None,
                        ..Default::default()
                    },
                    |_, cx| cx.new(|_| LateColors),
                )?;
                colors.update(cx, |_, window, _| {
                    match HasWindowHandle::window_handle(window)
                        .map_err(|error| anyhow!("Window handle: {error}"))?
                        .as_raw()
                    {
                        RawWindowHandle::Xcb(handle) => Ok(handle.window.get()),
                        _ => bail!("Expected XCB window"),
                    }
                })?
            })??;
            let (connection, _) = x11rb::connect(None)?;
            let geometry = connection.get_geometry(id)?.reply()?;
            let started = Instant::now();
            let deadline = started
                .checked_add(Duration::from_secs(3))
                .unwrap_or(started);
            loop {
                Timer::after(Duration::from_millis(50)).await;
                let image = connection
                    .get_image(
                        ImageFormat::Z_PIXMAP,
                        id,
                        0,
                        0,
                        geometry.width,
                        geometry.height,
                        u32::MAX,
                    )?
                    .reply()?;
                let colors: HashSet<_> = image
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| u32::from_ne_bytes(*pixel) & 0x00FF_FFFF)
                    .collect();
                if colors.contains(&0x11_33_55) && colors.contains(&0x55_77_99) {
                    return Ok(());
                }
                ensure!(
                    Instant::now() < deadline,
                    "Window opened after launch never presented its two colors ({}×{} pixels)",
                    geometry.width,
                    geometry.height
                );
            }
        }
    }
}
