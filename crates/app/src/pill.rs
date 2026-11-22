mod grille;

// Clippy's `allow-*-in-tests` settings recognize only a bare `cfg(test)`, so the platform gate is
// a separate attribute.
#[cfg(test)]
#[cfg(target_os = "linux")]
mod native_tests;
mod render;

use std::{
    fmt,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Pixels, Size, Task, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, point,
    prelude::*, px, size,
};
use speakeasy_core::{gesture::RECORDING_LIMIT, machine::Phase, motion::Spring};
use speakeasy_platform::{PILL_HEIGHT, PILL_WIDTH};
use speakeasy_show::{Ending, NarrationLoss};
use tokio::sync::watch;

use crate::{
    gpui_ext::{EntityUpdate, Presence, WindowUpdate, defer_presence, follow},
    runtime::Snapshot,
    theme::Theme,
    transcript::style::Style,
};

const METER_BARS: usize = 24;
const FRAME_INTERVAL: Duration = Duration::from_millis(5);

const SUBMITTED_FOR: Duration = Duration::from_millis(350);
const COPIED_FOR: Duration = Duration::from_millis(1600);
const NOTICE_FOR: Duration = Duration::from_secs(6);
const EMPTY_FOR: Duration = Duration::from_millis(1200);
const ERROR_FOR: Duration = Duration::from_secs(8);
const LONGEST_FEEDBACK: Duration = ERROR_FOR;
const DETAILS: &str = "Details in Settings";

const TRAY_HINT_FOR: Duration = Duration::from_secs(3);
const DRAW_HINT_FOR: Duration = Duration::from_secs(3);
const SECURE_INPUT_FOR: Duration = Duration::from_secs(5);
const DEFAULT_MICROPHONE_FOR: Duration = Duration::from_secs(4);

pub(crate) const COUNTDOWN: Duration = Duration::from_secs(30);
const SWEEP_DELAY: Duration = Duration::from_millis(250);
const SWEEP_PERIOD: Duration = Duration::from_millis(1200);

const RESTING_SIZE: Size<f32> = size(60.0, 30.0);
const RECORDING_WIDTH: f32 = 152.0;
const MESSAGE_WIDTH: f32 = 360.0;
const MESSAGE_SIZE: Size<f32> = size(MESSAGE_WIDTH, 64.0);
const HIDDEN_LIFT: f32 = 6.0;
const SHADOW_OFFSET: f32 = 4.0;
const SHADOW_BLUR: f32 = 16.0;
const SHADOW_ROOM: f32 = SHADOW_OFFSET + SHADOW_BLUR + HIDDEN_LIFT;

const _: () = assert!(
    MESSAGE_WIDTH + 2.0 * SHADOW_BLUR <= PILL_WIDTH as f32
        && MESSAGE_SIZE.height + SHADOW_ROOM <= PILL_HEIGHT as f32,
    "The capsule must fit the native pill window"
);

const WINDOW_SIZE: Size<Pixels> = size(px(PILL_WIDTH as f32), px(PILL_HEIGHT as f32));
/// How far above the display's bottom edge Linux keeps the pill, clear of a bottom panel.
///
/// GPUI reports no Linux work area; Windows and macOS move the pill onto theirs on every show.
const BOTTOM_CLEARANCE: Pixels = px(50.0);

const LID_OPEN: f32 = 0.0;
const LID_AJAR: f32 = 0.62;
const LID_CLOSED: f32 = 1.0;

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
    secure_input: fn() -> bool,
    secure_input_until: Option<Instant>,
    default_microphone_until: Option<Instant>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Opening {
    Startup,
    Reopened,
}

impl Pill {
    #[expect(
        clippy::too_many_arguments,
        reason = "open hands the new pill each feed, probe and preference it keeps beside GPUI's window and context; a parameter struct would only rename this list"
    )]
    fn new(
        updates: watch::Receiver<Snapshot>,
        levels: watch::Receiver<f32>,
        secure_input: fn() -> bool,
        reduced_motion: bool,
        theme: Theme,
        opening: Opening,
        window: &Window,
        cx: &Context<'_, Self>,
    ) -> Self {
        let now = Instant::now();
        let (snapshot, phase_at) = match opening {
            Opening::Startup => (Snapshot::default(), now),
            Opening::Reopened => (updates.borrow().clone(), inherited_phase_at(now)),
        };
        Self {
            snapshot,
            capsule: Capsule::hidden(),
            meter: std::array::from_fn(|_| Spring::new(0.0)),
            history: [0.0; METER_BARS],
            window: window.window_handle(),
            phase_at,
            tray_hint_until: None,
            draw_hint_until: None,
            secure_input,
            secure_input_until: None,
            default_microphone_until: None,
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

    pub(crate) fn set_reduced_motion(&mut self, reduced_motion: bool, cx: &mut Context<'_, Self>) {
        self.reduced_motion = reduced_motion;
        cx.notify();
    }

    pub(crate) fn set_theme(&mut self, theme: Theme, cx: &mut Context<'_, Self>) {
        self.theme = theme;
        cx.notify();
    }

    pub(crate) fn show_tray_hint(&mut self, cx: &mut Context<'_, Self>) -> bool {
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
        let new_session = snapshot.session != previous.session;
        let new_feedback = begins_feedback(previous, &snapshot);
        let state_changed = new_feedback
            || snapshot.message != previous.message
            || snapshot.hands_free != previous.hands_free
            || snapshot.for_script != previous.for_script
            || snapshot.default_microphone != previous.default_microphone
            || snapshot.show != previous.show
            || snapshot.ending != previous.ending
            || snapshot.narration_lost != previous.narration_lost;
        if new_feedback {
            self.phase_at = now;
            self.wake = None;
            self.tray_hint_until = None;
        }
        self.secure_input_until = secure_input_until(
            previous,
            &snapshot,
            now,
            self.secure_input_until,
            self.secure_input,
        );
        self.default_microphone_until = default_microphone_until(
            previous,
            &snapshot,
            now,
            self.default_microphone_until,
            self.secure_input_until,
        );
        self.draw_hint_until = draw_hint_until(
            previous,
            &snapshot,
            now,
            self.draw_hint_until,
            self.secure_input_until.max(self.default_microphone_until),
        );
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

    fn apply_meter(&mut self, level: f32, cx: &mut Context<'_, Self>) {
        if self.snapshot.phase != Phase::Recording {
            return;
        }
        let previous = self.history;
        self.history.rotate_left(1);
        let [.., newest] = &mut self.history;
        *newest = level;

        // Exact comparison: any tolerance would hide quiet speech. An animating pill paints new
        // levels on its pending frame.
        if previous.map(f32::to_bits) != self.history.map(f32::to_bits) && !self.animating {
            cx.notify();
        }
    }

    fn scene(&self, now: Instant) -> Scene {
        let remaining = |until: Option<Instant>| until?.checked_duration_since(now);
        Scene {
            phase: self.snapshot.phase,
            since_phase: now.duration_since(self.phase_at),
            tray_hint: remaining(self.tray_hint_until),
            draw_hint: remaining(self.draw_hint_until),
            secure_input: remaining(self.secure_input_until),
            default_microphone: remaining(self.default_microphone_until),
            hands_free: self.snapshot.hands_free,
            for_script: self.snapshot.for_script,
            show: self.snapshot.show,
            style: self.snapshot.style,
            ending: self.snapshot.ending,
            narration_lost: self.snapshot.narration_lost,
            clock: Clock::new(now.duration_since(self.snapshot.started)),
        }
    }

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
        let [.., newest] = self.history;
        for (spring, level) in self.meter.iter_mut().zip(self.history) {
            spring.target = match scene.phase {
                Phase::Recording if reduced_motion => newest,
                Phase::Recording => level,
                _ => 0.0,
            };
            let omega = meter_stiffness(scene.phase, spring.target > spring.value());
            moving |= advance(spring, omega);
        }
        moving
    }

    /// Settles every spring now, since a hidden native window receives no frames.
    ///
    /// The next entrance then starts at rest, not from wherever the fade-out left it moving.
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
        let presence = if visible {
            Presence::Passive
        } else {
            Presence::Hidden
        };
        self.visibility = Some(defer_presence(self.window, presence, cx));
    }

    /// Redraws on the first native frame at or after `frame_due`.
    ///
    /// This stays in display sync without a repeating timer or redraws between deadlines.
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
                cx.background_executor().timer(delay).await;
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
        self.lid.target = match scene.phase {
            Phase::Recording | Phase::Stopping => LID_OPEN,
            Phase::Processing => LID_AJAR,
            _ => LID_CLOSED,
        };
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

const FOR_SCRIPT: &str = "To script";

struct Scene {
    phase: Phase,
    since_phase: Duration,
    tray_hint: Option<Duration>,
    draw_hint: Option<Duration>,
    secure_input: Option<Duration>,
    default_microphone: Option<Duration>,
    hands_free: bool,
    for_script: bool,
    show: bool,
    style: Style,
    ending: Ending,
    narration_lost: Option<NarrationLoss>,
    clock: Clock,
}

impl Scene {
    fn is_shown(&self) -> bool {
        self.phase.is_active() || self.feedback().is_some() || self.tray_hint.is_some()
    }

    fn feedback(&self) -> Option<Duration> {
        feedback_remaining(self.phase, self.labels_copy(), self.since_phase)
    }

    fn labels_copy(&self) -> bool {
        self.show && self.ending == Ending::Finished && self.narration_lost.is_none()
    }

    fn is_capturing(&self) -> bool {
        matches!(self.phase, Phase::Recording | Phase::Stopping)
    }

    fn footer(&self) -> Option<&'static str> {
        (self.phase == Phase::Error || self.narration_lost.is_some()).then_some(DETAILS)
    }

    fn content(&self) -> Content {
        if self.tray_hint.is_some() {
            return Content::TrayHint;
        }
        if self.shows_secure_input() {
            return Content::SecureInput;
        }
        if self.shows_default_microphone() {
            return Content::DefaultMicrophone;
        }
        match self.phase {
            Phase::Error => Content::Error,
            Phase::Notice | Phase::Unchosen => Content::Notice,
            Phase::Copied if !self.labels_copy() => Content::Notice,
            Phase::Starting
            | Phase::Recording
            | Phase::Stopping
            | Phase::Processing
            | Phase::Done
            | Phase::Copied
            | Phase::Empty => Content::Slot,
            Phase::Idle | Phase::Cancelled => Content::Idle,
        }
    }

    fn shows_style(&self) -> bool {
        self.is_capturing() && !self.show && self.style != Style::Prose
    }

    fn label(&self) -> Option<&'static str> {
        if self.is_capturing() && self.for_script {
            Some(FOR_SCRIPT)
        } else {
            self.shows_style().then(|| self.style.label())
        }
    }

    fn shows_clock(&self) -> bool {
        self.is_capturing() && (self.hands_free || self.clock.near_limit || self.label().is_some())
    }

    fn shows_draw_hint(&self) -> bool {
        self.draw_hint.is_some() && self.shows_clock() && !self.clock.near_limit
    }

    fn shows_secure_input(&self) -> bool {
        self.secure_input.is_some() && self.is_capturing() && !self.clock.near_limit
    }

    fn shows_default_microphone(&self) -> bool {
        self.default_microphone.is_some() && self.phase.is_active() && !self.clock.near_limit
    }

    fn capsule_size(&self) -> Size<f32> {
        if !self.is_shown() {
            return RESTING_SIZE;
        }
        if self.tray_hint.is_some() {
            return size(MESSAGE_WIDTH, 36.0);
        }
        if self.shows_secure_input() || self.shows_default_microphone() {
            return MESSAGE_SIZE;
        }
        let mut bounds = match self.phase {
            Phase::Starting => size(172.0, 38.0),
            Phase::Recording | Phase::Stopping if self.clock.near_limit => size(262.0, 38.0),
            Phase::Recording | Phase::Stopping if self.shows_draw_hint() => size(300.0, 38.0),
            Phase::Recording | Phase::Stopping if self.shows_clock() => size(228.0, 38.0),
            Phase::Recording | Phase::Stopping => size(RECORDING_WIDTH, 38.0),
            Phase::Processing => size(104.0, 32.0),
            Phase::Done => size(76.0, 34.0),
            Phase::Copied if self.labels_copy() => size(150.0, 34.0),
            Phase::Empty => size(182.0, 36.0),
            Phase::Copied | Phase::Notice | Phase::Unchosen | Phase::Error => MESSAGE_SIZE,
            Phase::Idle | Phase::Cancelled => RESTING_SIZE,
        };
        if self.label().is_some() {
            bounds.width += 90.0;
        }
        bounds
    }

    fn wake_after(&self) -> Option<Duration> {
        self.feedback().or(self.tray_hint).or_else(|| {
            if self.is_capturing() {
                Some(
                    [self.draw_hint, self.secure_input, self.default_microphone]
                        .into_iter()
                        .flatten()
                        .fold(self.clock.next_tick, Duration::min),
                )
            } else if self.phase == Phase::Processing {
                SWEEP_DELAY
                    .checked_sub(self.since_phase)
                    .filter(|left| !left.is_zero())
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Content {
    TrayHint,
    SecureInput,
    DefaultMicrophone,
    Error,
    Notice,
    Slot,
    Idle,
}

impl Content {
    const fn is_inlaid(self) -> bool {
        matches!(
            self,
            Self::SecureInput | Self::DefaultMicrophone | Self::Error | Self::Notice | Self::Slot
        )
    }
}

struct Clock {
    seconds: u64,
    near_limit: bool,
    /// Counted from the recording's start, so a late wake cannot delay or skip a second.
    next_tick: Duration,
}

impl Clock {
    fn new(elapsed: Duration) -> Self {
        let seconds = elapsed.as_secs();
        Self {
            seconds,
            near_limit: elapsed >= RECORDING_LIMIT.saturating_sub(COUNTDOWN),
            next_tick: Duration::from_secs(seconds.saturating_add(1)).saturating_sub(elapsed),
        }
    }
}

impl fmt::Display for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.near_limit {
            let left = RECORDING_LIMIT.as_secs().saturating_sub(self.seconds);
            write!(f, "{}:{:02} left", left / 60, left % 60)
        } else {
            write!(f, "{}:{:02}", self.seconds / 60, self.seconds % 60)
        }
    }
}

pub(crate) fn open(
    updates: watch::Receiver<Snapshot>,
    levels: watch::Receiver<f32>,
    secure_input: fn() -> bool,
    reduced_motion: bool,
    theme: Theme,
    opening: Opening,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<Pill>> {
    let (kind, bounds) = placement(cx)?;
    let mut configured = Ok(());
    let pill = cx.open_window(
        WindowOptions {
            #[cfg(target_os = "linux")]
            app_id: Some(speakeasy_platform::APPLICATION_ID.into()),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            focus: false,
            show: false,
            kind,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            window.set_window_title("Speakeasy pill");
            // The pill lives as long as the app; refuse outside close requests, such as a tool
            // closing the process's main window.
            window.on_window_should_close(cx, |_, _| false);
            configured = configure(window);
            cx.new(|cx| {
                Pill::new(
                    updates,
                    levels,
                    secure_input,
                    reduced_motion,
                    theme,
                    opening,
                    window,
                    cx,
                )
            })
        },
    )?;
    if let Err(error) = configured {
        pill.update_if_open(cx, |_, window, _| window.remove_window());
        return Err(error);
    }
    Ok(pill)
}

fn placement(cx: &App) -> anyhow::Result<(WindowKind, Bounds<Pixels>)> {
    #[cfg(target_os = "linux")]
    match cx
        .try_global::<crate::application::Presentation>()
        .context("Speakeasy chose no display server before opening the pill")?
        .display_server
    {
        speakeasy_platform::DisplayServer::Wayland => {
            use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
            let overlay = LayerShellOptions {
                namespace: "speakeasy-pill".into(),
                layer: Layer::Overlay,
                anchor: Anchor::BOTTOM,
                margin: Some((px(0.0), px(0.0), BOTTOM_CLEARANCE, px(0.0))),
                keyboard_interactivity: KeyboardInteractivity::None,
                ..LayerShellOptions::default()
            };
            return Ok((
                WindowKind::LayerShell(overlay),
                Bounds::new(point(px(0.0), px(0.0)), WINDOW_SIZE),
            ));
        },
        speakeasy_platform::DisplayServer::X11 => {},
    }
    let screen = cx
        .primary_display()
        .context("No display available")?
        .bounds();
    let origin = point(
        screen.origin.x + (screen.size.width - WINDOW_SIZE.width) / 2.0,
        screen.bottom() - WINDOW_SIZE.height - BOTTOM_CLEARANCE,
    );
    Ok((WindowKind::PopUp, Bounds::new(origin, WINDOW_SIZE)))
}

#[cfg(target_os = "linux")]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Native pill configuration can fail on Windows and macOS; Linux shares that signature"
)]
fn configure(window: &Window) -> anyhow::Result<()> {
    window.set_input_region(Some(&[]));
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn configure(window: &Window) -> anyhow::Result<()> {
    crate::gpui_ext::native_window(window, "pill")?.configure_pill()
}

fn feedback_remaining(phase: Phase, labeled: bool, since_phase: Duration) -> Option<Duration> {
    let duration = match phase {
        Phase::Done => SUBMITTED_FOR,
        Phase::Copied if labeled => COPIED_FOR,
        Phase::Copied | Phase::Notice | Phase::Unchosen => NOTICE_FOR,
        Phase::Empty => EMPTY_FOR,
        Phase::Error => ERROR_FOR,
        _ => return None,
    };
    duration
        .checked_sub(since_phase)
        .filter(|left| !left.is_zero())
}

/// Backdates a reopened pill's phase so feedback from before the reopen does not replay.
fn inherited_phase_at(now: Instant) -> Instant {
    now.checked_sub(LONGEST_FEEDBACK).unwrap_or(now)
}

fn begins_feedback(previous: &Snapshot, snapshot: &Snapshot) -> bool {
    snapshot.session != previous.session
        || snapshot.reported != previous.reported
        || snapshot.phase != previous.phase
        || (snapshot.phase == Phase::Error && snapshot.message != previous.message)
}

fn secure_input_until(
    previous: &Snapshot,
    snapshot: &Snapshot,
    now: Instant,
    until: Option<Instant>,
    secure_input: impl FnOnce() -> bool,
) -> Option<Instant> {
    let same_session = snapshot.session == previous.session;
    let was_recording = same_session && previous.phase == Phase::Recording;
    if snapshot.phase == Phase::Recording && !was_recording {
        now.checked_add(SECURE_INPUT_FOR).filter(|_| secure_input())
    } else {
        until.filter(|_| same_session)
    }
}

fn default_microphone_until(
    previous: &Snapshot,
    snapshot: &Snapshot,
    now: Instant,
    until: Option<Instant>,
    secure_input_until: Option<Instant>,
) -> Option<Instant> {
    let same_session = snapshot.session == previous.session;
    if snapshot.default_microphone && !(same_session && previous.default_microphone) {
        secure_input_until
            .map_or(now, |notice| notice.max(now))
            .checked_add(DEFAULT_MICROPHONE_FOR)
    } else {
        until.filter(|_| same_session)
    }
}

fn draw_hint_until(
    previous: &Snapshot,
    snapshot: &Snapshot,
    now: Instant,
    until: Option<Instant>,
    notices_until: Option<Instant>,
) -> Option<Instant> {
    let until = until.filter(|_| snapshot.session == previous.session);
    if until.is_none() && snapshot.show && snapshot.phase == Phase::Recording {
        notices_until
            .map_or(now, |notice| notice.max(now))
            .checked_add(DRAW_HINT_FOR)
    } else {
        until
    }
}

const fn meter_stiffness(phase: Phase, rising: bool) -> f32 {
    match phase {
        Phase::Recording if rising => 160.0,
        Phase::Recording => 120.0,
        _ => 20.0,
    }
}

fn next_frame_deadline(previous: Instant, now: Instant) -> Instant {
    previous
        .checked_add(FRAME_INTERVAL)
        .filter(|next| *next > now)
        .or_else(|| now.checked_add(FRAME_INTERVAL))
        .unwrap_or(now)
}

#[cfg(test)]
mod tests;
