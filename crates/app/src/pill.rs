//! The floating pill: a borderless popup that animates the latest session snapshot. It only
//! presents; wakes and frames redraw the current snapshot and never publish session state or move a
//! capture or insertion deadline.

mod grille;
// Clippy's `allow-*-in-tests` settings recognize only a bare `cfg(test)`, so the platform gate is
// a separate attribute.
#[cfg(test)]
#[cfg(target_os = "linux")]
mod native_tests;
mod render;
mod symbol;

use std::time::{Duration, Instant};

use anyhow::Context as _;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Size, Task, Timer, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, point,
    prelude::*, px, size,
};
use speakeasy_core::{gesture::RECORDING_LIMIT, motion::Spring};
use speakeasy_platform::{PILL_HEIGHT, PILL_WIDTH};
use tokio::sync::watch;

use crate::{
    gpui_ext::{EntityUpdate, defer_native, follow, native_window},
    runtime::{Meter, Phase, Snapshot},
    theme::Theme,
};

const METER_BARS: usize = 24;
const FRAME_INTERVAL: Duration = Duration::from_millis(5);

const SUBMITTED_FOR: Duration = Duration::from_millis(350);
/// Long enough to read **Show copied**, which tells the user to paste.
const COPIED_FOR: Duration = Duration::from_millis(1600);
/// Long enough to read a two-line remedy for pasting copied text by hand.
const NOTICE_FOR: Duration = Duration::from_secs(6);
const EMPTY_FOR: Duration = Duration::from_millis(1200);
const ERROR_FOR: Duration = Duration::from_secs(8);
const TRAY_HINT_FOR: Duration = Duration::from_secs(3);
/// How long a recording that turns into a Show says how to draw, in place of its clock.
const DRAW_HINT_FOR: Duration = Duration::from_secs(3);

/// The final stretch of a recording during which the pill counts down to the limit.
pub(crate) const COUNTDOWN: Duration = Duration::from_secs(30);
const SWEEP_DELAY: Duration = Duration::from_millis(250);
const SWEEP_PERIOD: Duration = Duration::from_millis(1200);

const RESTING_SIZE: Size<f32> = size(60.0, 30.0);
const RECORDING_WIDTH: f32 = 152.0;
/// The widest capsule, which shows a message or the tray hint.
const MESSAGE_WIDTH: f32 = 360.0;
/// The tallest capsule: a two-line message above an optional footer.
const MESSAGE_SIZE: Size<f32> = size(MESSAGE_WIDTH, 64.0);
const HIDDEN_LIFT: f32 = 6.0;
const SHADOW_OFFSET: f32 = 4.0;
const SHADOW_BLUR: f32 = 16.0;
/// Room under the shown capsule for its drop shadow and for the `HIDDEN_LIFT` it sinks while hiding.
const SHADOW_ROOM: f32 = SHADOW_OFFSET + SHADOW_BLUR + HIDDEN_LIFT;
/// The fixed native window holds the widest capsule with its shadow's blur on both sides, and the
/// tallest with `SHADOW_ROOM` beneath it.
const _: () = assert!(
    MESSAGE_WIDTH + 2.0 * SHADOW_BLUR <= PILL_WIDTH as f32
        && MESSAGE_SIZE.height + SHADOW_ROOM <= PILL_HEIGHT as f32,
    "The capsule must fit the native pill window"
);
/// The slot row's horizontal padding.
const ROW_PADDING: f32 = 7.0;
/// The slot's horizontal inset inside the capsule: `ROW_PADDING` plus the capsule's one-pixel
/// border on each side.
const SLOT_SIDE_INSET: f32 = 2.0 * (ROW_PADDING + 1.0);
/// The slot's total vertical inset inside the capsule.
const SLOT_VERTICAL_INSET: f32 = 16.0;
/// The room the mark and clock leave before the row's padding at the capsule's rounded end, which
/// curves in closer to text than to the concentric slot.
const TRAILING_INSET: f32 = 7.0;
/// The window's clearance above the display's bottom edge. Windows and macOS move the pill onto
/// their work area; GPUI's X11 display has none, so on Linux this keeps it above a bottom panel.
const BOTTOM_CLEARANCE: f32 = 50.0;

const LID_OPEN: f32 = 0.0;
const LID_AJAR: f32 = 0.62;
const LID_CLOSED: f32 = 1.0;
/// The lid position from which a closed slot's label fades in.
const LABEL_FADE_FROM: f32 = 0.6;

const TRAY_HINT: &str = if cfg!(target_os = "macos") {
    "In the menu bar · Use Speakeasy’s menu to quit"
} else {
    "In the tray · Use Speakeasy’s menu to quit"
};
const DRAW_HINT: &str = "Hold Shift to draw";

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
    tray_hint_until: Option<Instant>,
    draw_hint_until: Option<Instant>,
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
    _levels: Task<()>,
}

impl Pill {
    fn new(
        updates: watch::Receiver<Snapshot>,
        levels: watch::Receiver<Meter>,
        reduced_motion: bool,
        theme: Theme,
        window: &Window,
        cx: &Context<'_, Self>,
    ) -> Self {
        let now = Instant::now();
        Self {
            snapshot: Snapshot::default(),
            capsule: Capsule::hidden(),
            meter: std::array::from_fn(|_| Spring::new(0.0)),
            history: [0.0; METER_BARS],
            window: window.window_handle(),
            phase_at: now,
            tray_hint_until: None,
            draw_hint_until: None,
            frame_at: now,
            frame_due: now,
            reduced_motion,
            theme,
            visible: false,
            animating: false,
            frame_pending: false,
            wake: None,
            visibility: None,
            _updates: follow(updates, cx, Snapshot::clone, Self::apply_snapshot),
            _levels: follow(levels, cx, |meter| *meter, Self::apply_meter),
        }
    }

    pub(crate) fn set_reduced_motion(&mut self, reduced_motion: bool) {
        self.reduced_motion = reduced_motion;
    }

    pub(crate) fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Shows the tray hint unless the pill is already visible; returns whether it did.
    pub(crate) fn tray_hint(&mut self, cx: &mut Context<'_, Self>) -> bool {
        if self.visible {
            return false;
        }
        self.tray_hint_until = Instant::now().checked_add(TRAY_HINT_FOR);
        self.wake = None;
        self.set_visible(true, cx);
        cx.notify();
        true
    }

    fn apply_snapshot(&mut self, snapshot: Snapshot, cx: &mut Context<'_, Self>) {
        let now = Instant::now();
        let previous = &self.snapshot;
        let new_session = snapshot.id != previous.id;
        let new_phase = new_session || snapshot.phase != previous.phase;
        let state_changed = new_phase
            || snapshot.hands_free != previous.hands_free
            || snapshot.show != previous.show
            || snapshot.message != previous.message;
        if new_phase || (snapshot.phase == Phase::Error && snapshot.message != previous.message) {
            self.phase_at = now;
            self.wake = None;
            self.tray_hint_until = None;
        }
        self.draw_hint_until = draw_hint_until(previous, &snapshot, now, self.draw_hint_until);
        if new_session {
            self.history.fill(0.0);
        }
        self.snapshot = snapshot;
        if state_changed {
            // A hidden native window never renders, so it cannot wait for `render` to show it.
            if !self.visible && self.scene(now).is_shown() {
                self.set_visible(true, cx);
            }
            cx.notify();
        }
    }

    /// Adds `meter` to the level history while the current snapshot records the session it
    /// measured; a level that arrives before its session's snapshot or after it ends is dropped.
    fn apply_meter(&mut self, meter: Meter, cx: &mut Context<'_, Self>) {
        let snapshot = &self.snapshot;
        if snapshot.phase != Phase::Recording
            || meter.epoch != snapshot.epoch
            || meter.session != snapshot.id
        {
            return;
        }
        let previous = self.history;
        self.history.rotate_left(1);
        if let Some(newest) = self.history.last_mut() {
            *newest = meter.level;
        }
        // Exact comparison: any tolerance would hide quiet speech. An animating pill paints new
        // levels on its pending frame.
        if previous.map(f32::to_bits) != self.history.map(f32::to_bits) && !self.animating {
            cx.notify();
        }
    }

    fn scene(&self, now: Instant) -> Scene {
        let phase = self.snapshot.phase;
        let since_phase = now.duration_since(self.phase_at);
        Scene {
            phase,
            since_phase,
            feedback: feedback_remaining(phase, self.snapshot.show, since_phase),
            tray_hint: self
                .tray_hint_until
                .and_then(|until| until.checked_duration_since(now)),
            draw_hint: self
                .draw_hint_until
                .and_then(|until| until.checked_duration_since(now)),
            hands_free: self.snapshot.hands_free,
            show: self.snapshot.show,
            clock: Clock::new(now.duration_since(self.snapshot.started)),
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
        let newest = self.history.last().copied().unwrap_or_default();
        for (spring, level) in self.meter.iter_mut().zip(self.history) {
            spring.target = match scene.phase {
                Phase::Recording if reduced_motion => newest,
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

    /// Hidden native windows receive no frames, so springs settle now: the next entrance starts at
    /// rest, not from wherever the fade-out left them moving.
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
        self.visibility = Some(defer_native(self.window, "pill", cx, move |native, _| {
            native.set_pill_visible(visible);
        }));
    }

    /// Redraws on the first native frame at or after `frame_due`, staying in display sync without a
    /// repeating timer or redraws between deadlines.
    fn request_deadline_frame(&mut self, window: &Window, cx: &Context<'_, Self>) {
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

    fn schedule_wake(&mut self, delay: Option<Duration>, cx: &Context<'_, Self>) {
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
}

impl Render for Pill {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
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
    tray_hint: Option<Duration>,
    /// Time left on a new Show's drawing hint.
    draw_hint: Option<Duration>,
    hands_free: bool,
    /// Whether the session records the screen as a Show.
    show: bool,
    clock: Clock,
}

impl Scene {
    fn is_shown(&self) -> bool {
        self.phase.is_active() || self.feedback.is_some() || self.tray_hint.is_some()
    }

    fn is_capturing(&self) -> bool {
        matches!(self.phase, Phase::Recording | Phase::Stopping)
    }

    fn has_slot(&self) -> bool {
        self.tray_hint.is_none()
            && match self.phase {
                Phase::Starting
                | Phase::Recording
                | Phase::Stopping
                | Phase::Processing
                | Phase::Done
                | Phase::Empty => true,
                Phase::Copied => self.show,
                Phase::Idle | Phase::Cancelled | Phase::Error => false,
            }
    }

    /// Copied dictation, whose message says how to paste it; a copied Show keeps its compact slot.
    fn is_notice(&self) -> bool {
        self.phase == Phase::Copied && !self.show
    }

    fn shows_clock(&self) -> bool {
        self.is_capturing() && (self.hands_free || self.clock.near_limit)
    }

    /// The drawing hint takes the clock's place, unless the clock is counting down to the limit.
    fn shows_draw_hint(&self) -> bool {
        self.draw_hint.is_some() && self.shows_clock() && !self.clock.near_limit
    }

    fn capsule_size(&self) -> Size<f32> {
        if !self.is_shown() {
            return RESTING_SIZE;
        }
        if self.tray_hint.is_some() {
            return size(MESSAGE_WIDTH, 36.0);
        }
        match self.phase {
            Phase::Starting => size(172.0, 38.0),
            Phase::Recording | Phase::Stopping if self.clock.near_limit => size(262.0, 38.0),
            Phase::Recording | Phase::Stopping if self.shows_draw_hint() => size(300.0, 38.0),
            Phase::Recording | Phase::Stopping if self.shows_clock() => size(228.0, 38.0),
            Phase::Recording | Phase::Stopping => size(RECORDING_WIDTH, 38.0),
            Phase::Processing => size(104.0, 32.0),
            Phase::Done => size(76.0, 34.0),
            Phase::Copied if self.show => size(150.0, 34.0),
            Phase::Empty => size(182.0, 36.0),
            Phase::Copied | Phase::Error => MESSAGE_SIZE,
            Phase::Idle | Phase::Cancelled => RESTING_SIZE,
        }
    }

    fn wake_after(&self) -> Option<Duration> {
        self.feedback.or(self.tray_hint).or_else(|| {
            if self.is_capturing() {
                let tick = self.clock.next_tick;
                Some(self.draw_hint.map_or(tick, |left| left.min(tick)))
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

/// The recording's elapsed seconds, and whether it has reached the countdown before its limit.
struct Clock {
    seconds: u64,
    near_limit: bool,
    /// Until the recording's next whole second, when the shown time changes. Counting from the
    /// recording's start, not the last wake, keeps late wakes from delaying or skipping a second.
    next_tick: Duration,
}

impl Clock {
    fn new(elapsed: Duration) -> Self {
        let seconds = elapsed.as_secs();
        let limit = RECORDING_LIMIT.as_secs();
        Self {
            seconds,
            near_limit: seconds >= limit.saturating_sub(COUNTDOWN.as_secs()),
            next_tick: Duration::from_secs(1)
                .saturating_sub(Duration::from_nanos(u64::from(elapsed.subsec_nanos()))),
        }
    }

    /// The elapsed time, or the time left once the countdown begins.
    fn text(&self) -> String {
        if self.near_limit {
            let left = RECORDING_LIMIT.as_secs().saturating_sub(self.seconds);
            format!("{}:{:02} left", left / 60, left % 60)
        } else {
            format!("{}:{:02}", self.seconds / 60, self.seconds % 60)
        }
    }
}

/// Opens the pill, hidden, to present `updates` and `levels`. It starts centered above the primary
/// display's bottom edge; Windows and macOS move it to the bottom of the user's current display
/// each time it shows.
///
/// Fails without a display, when the platform cannot open the window, or when the window cannot be
/// made nonactivating and click-through.
pub(crate) fn open(
    updates: watch::Receiver<Snapshot>,
    levels: watch::Receiver<Meter>,
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
            // The pill lives as long as the app; a close request from outside, such as a tool
            // closing the process's main window, is refused.
            window.on_window_should_close(cx, |_, _| false);
            configured = native_window(window, "pill").and_then(|native| native.configure_pill());
            cx.new(|cx| Pill::new(updates, levels, reduced_motion, theme, window, cx))
        },
    )?;
    configured?;
    Ok(pill)
}

fn feedback_remaining(phase: Phase, show: bool, since_phase: Duration) -> Option<Duration> {
    let duration = match phase {
        Phase::Done => SUBMITTED_FOR,
        Phase::Copied if show => COPIED_FOR,
        Phase::Copied => NOTICE_FOR,
        Phase::Empty => EMPTY_FOR,
        Phase::Error => ERROR_FOR,
        _ => return None,
    };
    duration
        .checked_sub(since_phase)
        .filter(|left| !left.is_zero())
}

/// When the drawing hint ends, given the snapshot that replaces `previous` and the deadline `until`
/// kept so far. A session's hint starts the first time it records as a Show, so S pressed while the
/// microphone opens still gets the full hint, and the session's later snapshots keep its deadline.
fn draw_hint_until(
    previous: &Snapshot,
    snapshot: &Snapshot,
    now: Instant,
    until: Option<Instant>,
) -> Option<Instant> {
    let until = until.filter(|_| snapshot.id == previous.id);
    if until.is_none() && snapshot.show && snapshot.phase == Phase::Recording {
        now.checked_add(DRAW_HINT_FOR)
    } else {
        until
    }
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
            feedback_remaining(Phase::Done, false, Duration::ZERO),
            Some(SUBMITTED_FOR)
        );
        assert_eq!(feedback_remaining(Phase::Done, false, SUBMITTED_FOR), None);
        assert!(
            feedback_remaining(Phase::Copied, true, SUBMITTED_FOR).is_some(),
            "A copied Show stays long enough to read"
        );
        assert_eq!(feedback_remaining(Phase::Copied, true, COPIED_FOR), None);
        assert!(
            feedback_remaining(Phase::Copied, false, COPIED_FOR).is_some(),
            "A paste remedy stays longer than the Show's two words"
        );
        assert_eq!(feedback_remaining(Phase::Copied, false, NOTICE_FOR), None);
        assert_eq!(
            feedback_remaining(Phase::Recording, false, Duration::ZERO),
            None
        );
        assert_eq!(
            feedback_remaining(Phase::Recording, true, SUBMITTED_FOR),
            None
        );
        assert_eq!(
            feedback_remaining(Phase::Cancelled, false, Duration::ZERO),
            None
        );
        assert_eq!(feedback_remaining(Phase::Error, false, ERROR_FOR), None);
    }

    #[test]
    fn a_new_show_says_how_to_draw_in_place_of_its_clock_until_the_hint_ends() {
        let show = |seconds, draw_hint| Scene {
            phase: Phase::Recording,
            since_phase: Duration::ZERO,
            feedback: None,
            tray_hint: None,
            draw_hint,
            hands_free: true,
            show: true,
            clock: Clock::new(Duration::from_secs(seconds)),
        };
        let left = Duration::from_millis(400);
        let hinted = show(2, Some(left));
        assert!(hinted.shows_draw_hint());
        assert!(
            hinted.capsule_size().width > show(2, None).capsule_size().width,
            "The capsule widens for the hint"
        );
        assert_eq!(
            hinted.wake_after(),
            Some(left),
            "The clock returns as the hint ends"
        );
        assert!(
            !show(280, Some(left)).shows_draw_hint(),
            "The countdown to the limit outranks the hint"
        );
        let stopping = Scene {
            phase: Phase::Stopping,
            hands_free: false,
            ..show(2, Some(left))
        };
        assert!(
            !stopping.shows_draw_hint(),
            "A Show that stops drops the hint with its clock"
        );
    }

    #[test]
    fn clock_wakes_land_on_whole_seconds_of_the_recording() {
        let recording = |elapsed| Scene {
            phase: Phase::Recording,
            since_phase: Duration::ZERO,
            feedback: None,
            tray_hint: None,
            draw_hint: None,
            hands_free: true,
            show: false,
            clock: Clock::new(elapsed),
        };
        assert_eq!(
            recording(Duration::from_millis(2_300)).wake_after(),
            Some(Duration::from_millis(700)),
            "A frame drawn late into a second waits only for the next one"
        );
        assert_eq!(
            recording(Duration::from_secs(2)).wake_after(),
            Some(Duration::from_secs(1))
        );
    }

    #[test]
    fn the_drawing_hint_starts_when_a_show_records_and_ends_with_its_session() {
        let snapshot = |id, phase, show| Snapshot {
            id,
            phase,
            hands_free: show,
            show,
            ..Snapshot::default()
        };
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let full = now.checked_add(DRAW_HINT_FOR);
        let held = snapshot(1, Phase::Recording, false);
        let show = snapshot(1, Phase::Recording, true);
        assert_eq!(
            draw_hint_until(&held, &show, now, None),
            full,
            "S during a hold starts the hint"
        );
        let republished = Snapshot {
            message: "Republished".into(),
            ..show.clone()
        };
        assert_eq!(
            draw_hint_until(&show, &republished, later, full),
            full,
            "A republished Show keeps its deadline"
        );
        let opening = snapshot(1, Phase::Starting, true);
        assert_eq!(
            draw_hint_until(&snapshot(1, Phase::Starting, false), &opening, now, None),
            None,
            "The hint waits for the microphone"
        );
        assert_eq!(
            draw_hint_until(&opening, &show, later, None),
            later.checked_add(DRAW_HINT_FOR),
            "A Show that starts recording gets the full hint"
        );
        let hands_free = Snapshot {
            hands_free: true,
            ..held.clone()
        };
        assert_eq!(
            draw_hint_until(&held, &hands_free, now, None),
            None,
            "Hands-free dictation is not a Show"
        );
        assert_eq!(
            draw_hint_until(&show, &snapshot(2, Phase::Starting, false), later, full),
            None,
            "A new session drops the hint"
        );
    }

    #[test]
    fn limit_countdown_is_clamped_and_only_appears_in_the_last_thirty_seconds() {
        let clock = |seconds| {
            let clock = Clock::new(Duration::from_secs(seconds));
            (clock.near_limit, clock.text())
        };
        assert_eq!(clock(269), (false, "4:29".into()));
        assert_eq!(clock(270), (true, "0:30 left".into()));
        assert_eq!(clock(305), (true, "0:00 left".into()));
    }
}
