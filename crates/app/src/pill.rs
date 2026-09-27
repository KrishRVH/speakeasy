use crate::runtime::{Phase, Snapshot};
use gpui::{prelude::*, *};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use raw_window_handle::HasWindowHandle;
use speakeasy_core::motion::Spring;
use std::time::{Duration, Instant};

pub struct Pill {
    snapshot: Snapshot,
    width: Spring,
    height: Spring,
    opacity: Spring,
    meter: [Spring; 24],
    history: [f32; 24],
    frame_at: Instant,
    phase_at: Instant,
    reduced: bool,
    visible: bool,
    animating: bool,
    wake: Option<Task<()>>,
    _updates: Task<()>,
}

impl Pill {
    pub fn set_reduced(&mut self, reduced: bool) {
        self.reduced = reduced;
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
                        let old_history = pill.history;
                        let state_changed = snapshot.phase != pill.snapshot.phase
                            || snapshot.id != pill.snapshot.id
                            || snapshot.hands_free != pill.snapshot.hands_free
                            || snapshot.message != pill.snapshot.message;
                        if snapshot.phase != pill.snapshot.phase || snapshot.id != pill.snapshot.id
                        {
                            pill.phase_at = Instant::now();
                            pill.frame_at = Instant::now();
                            pill.wake = None;
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
                            && matches!(
                                snapshot.phase,
                                Phase::Starting
                                    | Phase::Recording
                                    | Phase::Stopping
                                    | Phase::Processing
                                    | Phase::Error
                            )
                            && !pill.visible
                        {
                            #[cfg(any(target_os = "windows", target_os = "macos"))]
                            set_visible(window, true, cx);
                            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
                            let _ = window;
                            pill.visible = true;
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
            meter: std::array::from_fn(|_| Spring::new(0.0)),
            history: [0.0; 24],
            frame_at: Instant::now(),
            phase_at: Instant::now(),
            reduced,
            visible: false,
            animating: false,
            wake: None,
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
        let recording = matches!(phase, Phase::Recording | Phase::Stopping);
        let error = phase == Phase::Error && elapsed < Duration::from_secs(8);
        let show = matches!(
            phase,
            Phase::Starting | Phase::Recording | Phase::Stopping | Phase::Processing
        ) || error;
        let (width, height) = match phase {
            Phase::Recording | Phase::Stopping => (
                if self.snapshot.hands_free {
                    188.0
                } else {
                    148.0
                },
                36.0,
            ),
            Phase::Processing => (72.0, 24.0),
            Phase::Error => (360.0, 44.0),
            _ => (56.0, 28.0),
        };
        self.width.target = width;
        self.height.target = height;
        self.opacity.target = if show { 1.0 } else { 0.0 };
        let mut moving = false;
        for spring in [&mut self.width, &mut self.height, &mut self.opacity] {
            if self.reduced {
                spring.snap();
            } else {
                spring.step(dt);
            }
            moving |= !spring.settled();
        }
        for (index, spring) in self.meter.iter_mut().enumerate() {
            spring.target = if recording {
                if self.reduced {
                    self.snapshot.level
                } else {
                    self.history[index]
                }
            } else {
                0.0
            };
            spring.step(dt);
            moving |= !spring.settled();
        }
        let visible = show || self.opacity.value > 0.001;
        if visible != self.visible {
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            set_visible(window, visible, cx);
            self.visible = visible;
        }
        self.animating = moving || (phase == Phase::Processing && !self.reduced);
        if self.animating {
            window.request_animation_frame();
        }
        let wake_after = if error {
            Some(Duration::from_secs(8).saturating_sub(elapsed))
        } else if recording && self.snapshot.hands_free {
            Some(Duration::from_secs(1))
        } else if phase == Phase::Processing && self.reduced && elapsed < Duration::from_millis(250)
        {
            Some(Duration::from_millis(250) - elapsed)
        } else {
            None
        };
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
        let dot_color = if recording {
            0xff4f2e
        } else if error {
            0xf5a524
        } else {
            0x95969c
        };
        let content = if error {
            div()
                .w_full()
                .text_size(px(12.0))
                .text_color(rgb(0xe7e7e9))
                .child(self.snapshot.message.clone())
                .into_any_element()
        } else if phase == Phase::Processing {
            let progress =
                ((elapsed.as_secs_f32() * std::f32::consts::TAU / 1.2).sin() + 1.0) / 2.0;
            div()
                .w(px(40.0))
                .h(px(2.0))
                .rounded_full()
                .bg(rgb(0x3d3e44))
                .when(elapsed >= Duration::from_millis(250), |line| {
                    line.child(
                        div()
                            .w(px(if self.reduced { 40.0 } else { 8.0 }))
                            .h(px(2.0))
                            .rounded_full()
                            .ml(px(if self.reduced { 0.0 } else { progress * 32.0 }))
                            .bg(rgb(0xc1c2c8)),
                    )
                })
                .into_any_element()
        } else {
            let seconds = self.snapshot.started.elapsed().as_secs();
            div()
                .flex()
                .items_center()
                .gap(px(9.0))
                .child(
                    div()
                        .size(px(8.0))
                        .rounded_full()
                        .bg(rgb(dot_color))
                        .when(self.snapshot.hands_free, |dot| {
                            dot.border_1().border_color(rgb(0xffb8a8))
                        }),
                )
                .when(recording, |row| {
                    row.child(meter(
                        self.meter.each_ref().map(|spring| spring.value),
                        self.reduced,
                    ))
                    .when(self.snapshot.hands_free, |row| {
                        row.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(0xa9aab2))
                                .child(format!("{}:{:02}", seconds / 60, seconds % 60)),
                        )
                    })
                })
                .into_any_element()
        };
        div()
            .size_full()
            .flex()
            .items_end()
            .justify_center()
            .pb(px(20.0))
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
                    .border_color(rgba(0xffffff17))
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
fn set_visible(window: &Window, visible: bool, cx: &mut App) {
    let handle = Window::window_handle(window);
    cx.spawn(async move |cx| {
        let raw = cx.update_window(handle, |_, window, _| {
            HasWindowHandle::window_handle(window).map(|handle| handle.as_raw())
        });
        if let Ok(Ok(raw)) = raw {
            speakeasy_platform::set_pill_visible(raw, visible);
        }
    })
    .detach();
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
