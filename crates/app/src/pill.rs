use crate::runtime::{Phase, Snapshot};
use crate::theme::{Palette, Theme, alpha, mix};
use gpui::{prelude::*, *};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use raw_window_handle::HasWindowHandle;
use speakeasy_core::{gesture::RECORDING_LIMIT, motion::Spring};
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(5); // 200 FPS
const SUBMITTED_FOR: Duration = Duration::from_millis(350);
const EMPTY_FOR: Duration = Duration::from_millis(1200);
const ERROR_FOR: Duration = Duration::from_secs(8);

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

fn next_frame_deadline(previous: Instant, now: Instant) -> Instant {
    let next = previous + FRAME_INTERVAL;
    if next > now {
        next
    } else {
        now + FRAME_INTERVAL
    }
}

fn recording_clock(seconds: u64) -> (bool, String) {
    let limit = RECORDING_LIMIT.as_secs();
    if seconds >= limit - 30 {
        (true, format!("0:{:02} left", limit.saturating_sub(seconds)))
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
    lid: Spring,
    meter: [Spring; 24],
    history: [f32; 24],
    frame_at: Instant,
    phase_at: Instant,
    hint_until: Option<Instant>,
    reduced: bool,
    theme: Theme,
    visible: bool,
    animating: bool,
    frame_pending: bool,
    frame_due: Instant,
    wake: Option<Task<()>>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    visibility: Option<Task<()>>,
    _updates: Task<()>,
}

impl Pill {
    // Check the deadline on native frames, preserving display synchronization
    // without redrawing between deadlines or creating a repeating timer.
    fn request_frame(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.frame_pending {
            return;
        }
        self.frame_pending = true;
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |pill, cx| {
                pill.frame_pending = false;
                if !pill.animating {
                    return;
                }
                if Instant::now() >= pill.frame_due {
                    cx.notify();
                } else {
                    pill.request_frame(window, cx);
                }
            });
        });
    }

    pub fn set_reduced(&mut self, reduced: bool) {
        self.reduced = reduced;
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
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
        theme: Theme,
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
                        // Moving meters paint the latest history on their pending
                        // frame. Session changes and settled meters redraw immediately.
                        if state_changed || (old_history != pill.history && !pill.animating) {
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
            width: Spring::new(60.0),
            height: Spring::new(30.0),
            opacity: Spring::new(0.0),
            lift: Spring::new(6.0),
            waveform: Spring::new(0.0),
            lid: Spring::new(1.0),
            meter: std::array::from_fn(|_| Spring::new(0.0)),
            history: [0.0; 24],
            frame_at: Instant::now(),
            phase_at: Instant::now(),
            hint_until: None,
            reduced,
            theme,
            visible: false,
            animating: false,
            frame_pending: false,
            frame_due: Instant::now(),
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
            (60.0, 30.0)
        } else if hint.is_some() {
            (360.0, 36.0)
        } else {
            match phase {
                Phase::Starting => (172.0, 38.0),
                Phase::Recording | Phase::Stopping => (
                    if near_limit {
                        262.0
                    } else if show_clock {
                        228.0
                    } else {
                        152.0
                    },
                    38.0,
                ),
                Phase::Processing => (104.0, 32.0),
                Phase::Done => (76.0, 34.0),
                Phase::Empty => (182.0, 36.0),
                Phase::Error => (360.0, 64.0),
                _ => (60.0, 30.0),
            }
        };
        self.width.target = width;
        self.height.target = height;
        self.opacity.target = if show { 1.0 } else { 0.0 };
        self.lift.target = if show { 0.0 } else { 6.0 };
        self.waveform.target = if capturing { 1.0 } else { 0.0 };
        // The lid opens only while capture is live and half-closes while processing.
        self.lid.target = match phase {
            Phase::Recording | Phase::Stopping => 0.0,
            Phase::Processing => 0.62,
            _ => 1.0,
        };
        let mut moving = false;
        for (spring, response) in [
            (&mut self.width, 28.0),
            (&mut self.height, 28.0),
            (&mut self.opacity, 40.0),
            (&mut self.lift, 32.0),
            (&mut self.waveform, 32.0),
            (&mut self.lid, 24.0),
        ] {
            if self.reduced {
                spring.snap();
            } else {
                spring.step(dt, response);
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
                spring.step(
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
                &mut self.lid,
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
        self.frame_due = next_frame_deadline(self.frame_due, now);
        if self.animating {
            self.request_frame(window, cx);
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
        let palette = self.theme.palette();
        let (width, height) = (self.width.value, self.height.value);
        let slot = hint.is_none()
            && matches!(
                phase,
                Phase::Starting
                    | Phase::Recording
                    | Phase::Stopping
                    | Phase::Processing
                    | Phase::Done
                    | Phase::Empty
            );
        let content = if hint.is_some() {
            div()
                .text_size(px(12.0))
                .child(if cfg!(target_os = "macos") {
                    "In the menu bar · Use Speakeasy’s menu to quit"
                } else {
                    "In the tray · Use Speakeasy’s menu to quit"
                })
                .into_any_element()
        } else if phase == Phase::Error {
            div()
                .w_full()
                .px(px(14.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(symbol(Symbol::Attention, palette.warn))
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
                .into_any_element()
        } else if slot {
            // The slot keeps its recording width while the clock widens the pill.
            let extra = if show_clock {
                (width - 152.0).max(0.0)
            } else {
                0.0
            };
            let slot_height = (height - 16.0).max(4.0);
            let label = match phase {
                Phase::Starting => Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(symbol(Symbol::Microphone, palette.muted))
                        .child(div().text_size(px(12.0)).child("Opening mic…"))
                        .into_any_element(),
                ),
                Phase::Empty => Some(
                    div()
                        .text_size(px(12.0))
                        .child("No speech detected")
                        .into_any_element(),
                ),
                Phase::Done => Some(symbol(Symbol::Check, palette.lamp).into_any_element()),
                _ => None,
            };
            let grille = Grille {
                lid: self.lid.value.clamp(0.0, 1.0),
                wave: self.waveform.value.clamp(0.0, 1.0),
                levels: self.meter.each_ref().map(|spring| spring.value),
                reduced: self.reduced,
                sweep: (phase == Phase::Processing && elapsed >= Duration::from_millis(250)).then(
                    || {
                        if self.reduced {
                            0.5
                        } else {
                            ((elapsed.as_secs_f32() * std::f32::consts::TAU / 1.2).sin() + 1.0)
                                / 2.0
                        }
                    },
                ),
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
                        .w(px((width - 16.0 - extra).max(6.0)))
                        .h(px(slot_height))
                        .rounded(px(slot_height / 2.0))
                        .bg(alpha(0x000000, 0.5))
                        .border_1()
                        .border_color(if grille.wave > 0.01 {
                            alpha(palette.live, 0.45 * grille.wave)
                        } else {
                            alpha(palette.ink, 0.08)
                        })
                        .child(
                            canvas(
                                |_, _, _| (),
                                move |bounds, _, window, _| grille.paint(bounds, window),
                            )
                            .size_full(),
                        )
                        .when_some(label, |slot, label| {
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
                .when(show_clock, |row| {
                    row.when(self.snapshot.hands_free, |row| {
                        row.child(symbol(Symbol::Lock, palette.muted))
                    })
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(if near_limit {
                                palette.warn
                            } else {
                                palette.muted
                            }))
                            .child(clock),
                    )
                })
                .into_any_element()
        } else {
            div()
                .w(px(18.0))
                .h(px(2.0))
                .rounded_full()
                .bg(alpha(palette.ink, 0.5))
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .items_end()
            .justify_center()
            // Room below the capsule for its whole shadow; the window sits 2 DIP
            // above the work area, placing the capsule 28 DIP above its edge.
            .pb(px(26.0 - self.lift.value))
            .child(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(width))
                    .h(px(height))
                    .rounded(px(height / 2.0))
                    .bg(alpha(palette.door, 0.96))
                    .border_1()
                    .border_color(alpha(palette.ink, 0.13))
                    .text_color(rgb(palette.ink))
                    .shadow(vec![BoxShadow {
                        color: rgba(0x00000052).into(),
                        offset: point(px(0.0), px(4.0)),
                        blur_radius: px(16.0),
                        spread_radius: px(0.0),
                    }])
                    .overflow_hidden()
                    .opacity(self.opacity.value.clamp(0.0, 1.0))
                    .when(slot && height > 22.0, |capsule| {
                        // A fine brass inlay; ornament stays still while state moves.
                        capsule.child(
                            div()
                                .absolute()
                                .top(px(2.5))
                                .left(px(2.5))
                                .right(px(2.5))
                                .bottom(px(2.5))
                                .rounded(px(height / 2.0 - 3.5))
                                .border_1()
                                .border_color(alpha(palette.lamp, 0.16)),
                        )
                    })
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

// The door slot: a symmetric grille of measured levels behind a sliding lid.
// Painting shares one layout element; bar geometry is independent of layout
// and hit testing as the audio level changes.
#[derive(Clone, Copy)]
struct Grille {
    lid: f32,
    wave: f32,
    levels: [f32; 24],
    reduced: bool,
    sweep: Option<f32>,
    palette: &'static Palette,
}

impl Grille {
    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let palette = self.palette;
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let radius = h / 2.0;
        let open = w * (1.0 - self.lid);
        let rect = |left: f32, top: f32, width: f32, height: f32| {
            Bounds::new(point(px(left), px(top)), size(px(width), px(height)))
        };
        if self.wave > 0.01 {
            // Light behind the door brightens with the voice.
            window.paint_quad(
                fill(
                    bounds,
                    alpha(palette.live, (0.1 + self.levels[23] * 0.22) * self.wave),
                )
                .corner_radii(px(radius)),
            );
            let color = alpha(palette.live, self.wave);
            let max = h - 6.0;
            let mask = ContentMask {
                bounds: rect(x, y, open, h),
            };
            window.with_content_mask(Some(mask), |window| {
                if self.reduced {
                    let bar = 2.0 + self.levels[23] * max;
                    window.paint_quad(
                        fill(rect(x + 9.0, y + (h - bar) / 2.0, w - 18.0, bar), color)
                            .corner_radii(px(bar.min(6.0) / 2.0)),
                    );
                    return;
                }
                let count = ((w - 12.0) / 4.0).floor().max(1.0) as usize;
                let start = x + (w - (count as f32 * 4.0 - 2.0)) / 2.0;
                let middle = (count as f32 - 1.0) / 2.0;
                for index in 0..count {
                    // Newest level at the center, older levels mirrored outward.
                    let distance = ((index as f32 - middle).abs().round() as usize).min(23);
                    let left = start + index as f32 * 4.0;
                    let inset = (x + radius - left - 1.0)
                        .max(left + 1.0 - (x + w - radius))
                        .max(0.0);
                    let chord = 2.0 * (radius * radius - inset * inset).max(0.0).sqrt() - 3.0;
                    let bar = (2.0 + self.levels[23 - distance] * max).min(chord.max(2.0));
                    window.paint_quad(
                        fill(rect(left, y + (h - bar) / 2.0, 2.0, bar), color)
                            .corner_radii(px(1.0)),
                    );
                }
            });
        }
        if let Some(position) = self.sweep
            && open > 16.0
        {
            let glow = rect(x + 4.0 + position * (open - 18.0), y + 3.0, 10.0, h - 6.0);
            let corners = Corners::all(px(((h - 6.0) / 2.0).min(5.0)));
            window.paint_shadows(
                glow,
                corners,
                &[BoxShadow {
                    color: alpha(palette.lamp, 0.6).into(),
                    offset: point(px(0.0), px(0.0)),
                    blur_radius: px(6.0),
                    spread_radius: px(0.0),
                }],
            );
            window.paint_quad(fill(glow, alpha(palette.lamp, 0.85)).corner_radii(corners));
        }
        let cover = w - open;
        if cover >= 1.0 {
            let left = x + open;
            // A nearly open lid follows the curve of the slot's end.
            let height = if cover < radius {
                2.0 * (radius * radius - (radius - cover).powi(2)).max(0.0).sqrt()
            } else {
                h
            };
            let top = y + (h - height) / 2.0;
            let leading = px((radius - open).max(0.0));
            let trailing = px((height / 2.0).min(cover));
            window.paint_quad(
                fill(
                    rect(left, top, cover, height),
                    linear_gradient(
                        180.0,
                        linear_color_stop(rgb(mix(palette.raise, palette.ink, 0.06)), 0.0),
                        linear_color_stop(rgb(palette.door), 1.0),
                    ),
                )
                .corner_radii(Corners {
                    top_left: leading,
                    top_right: trailing,
                    bottom_right: trailing,
                    bottom_left: leading,
                }),
            );
            if open >= 0.5 {
                window.paint_quad(fill(rect(left, top, 1.0, height), alpha(palette.ink, 0.2)));
            }
            if cover > 20.0 && h > 12.0 {
                keystone(
                    window,
                    left + 7.0,
                    y + radius,
                    2.4,
                    alpha(palette.lamp, 0.8),
                );
            }
        }
    }
}

pub fn keystone(window: &mut Window, x: f32, y: f32, radius: f32, color: Rgba) {
    let mut path = PathBuilder::fill();
    path.move_to(point(px(x), px(y - radius)));
    path.line_to(point(px(x + radius), px(y)));
    path.line_to(point(px(x), px(y + radius)));
    path.line_to(point(px(x - radius), px(y)));
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
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
    theme: Theme,
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
            cx.new(|cx| Pill::new(updates, reduced, theme, window, cx))
        },
    )?;
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    native_result?;
    Ok(handle)
}

#[cfg(test)]
mod tests {
    use super::{
        ERROR_FOR, FRAME_INTERVAL, SUBMITTED_FOR, feedback_remaining, next_frame_deadline,
        recording_clock,
    };
    use crate::runtime::Phase;
    use std::time::{Duration, Instant};

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
        // Slower displays remain eligible on every native frame.
        let at_60_hz = deadline + Duration::from_nanos(1_000_000_000 / 60);
        assert!(next_frame_deadline(deadline, deadline) < at_60_hz);
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
        assert_eq!(recording_clock(269), (false, "4:29".into()));
        assert_eq!(recording_clock(270), (true, "0:30 left".into()));
        assert_eq!(recording_clock(305), (true, "0:00 left".into()));
    }
}
