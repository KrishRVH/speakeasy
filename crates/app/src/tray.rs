#[cfg(any(target_os = "windows", target_os = "macos"))]
use crate::shell;
#[cfg(target_os = "linux")]
mod linux;
use crate::{
    runtime::Phase,
    shell::Services,
    status::{self, Indicator},
    theme::Theme,
};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use gpui::{App, Global, Task};
#[cfg(target_os = "linux")]
pub use linux::{Tray, install};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use speakeasy_platform::Input;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use tray_icon::{
    Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub struct Tray {
    _icon: TrayIcon,
    _events: Task<()>,
    _updates: Task<()>,
}
#[cfg(any(target_os = "windows", target_os = "macos"))]
impl Global for Tray {}

#[derive(PartialEq, Eq)]
struct TrayState {
    indicator: Indicator,
    theme: Theme,
    description: String,
    running: bool,
    pausing: bool,
    capturing: bool,
    active: bool,
    ready: bool,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn install(cx: &mut App) -> anyhow::Result<()> {
    let menu = Menu::new();
    let status = MenuItem::new("Loading…", false, None);
    let pause = MenuItem::new("Pause dictation", true, None);
    let toggle = MenuItem::new("Start dictation", false, None);
    let cancel = MenuItem::new("Cancel dictation", false, None);
    let settings = MenuItem::new("Settings…", true, None);
    let quit = MenuItem::new("Quit Speakeasy", true, None);
    menu.append_items(&[
        &status,
        &PredefinedMenuItem::separator(),
        &pause,
        &toggle,
        &cancel,
        &PredefinedMenuItem::separator(),
        &settings,
        &quit,
    ])?;
    let icon = TrayIconBuilder::new()
        .with_tooltip("Speakeasy")
        .with_icon_as_template(true)
        .with_icon(draw_icon(
            Indicator::Paused,
            cx.global::<Services>().config.theme,
        )?)
        .with_menu(Box::new(menu))
        .build()?;

    let (events, incoming) = async_channel::bounded(16);
    let sender = events.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = sender.try_send(Some(event.id));
    }));
    TrayIconEvent::set_event_handler(Some(move |event| {
        if matches!(
            event,
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        ) {
            let _ = events.try_send(None);
        }
    }));
    let pause_id = pause.id().clone();
    let toggle_id = toggle.id().clone();
    let cancel_id = cancel.id().clone();
    let settings_id = settings.id().clone();
    let quit_id = quit.id().clone();
    let event_task = cx.spawn(async move |cx| {
        while let Ok(id) = incoming.recv().await {
            if cx
                .update(|cx| {
                    if id.is_none() || id.as_ref() == Some(&settings_id) {
                        shell::reveal(cx);
                    } else if id.as_ref() == Some(&pause_id) {
                        shell::toggle_enabled(cx);
                    } else if id.as_ref() == Some(&toggle_id) {
                        shell::send(Input::Toggle, cx);
                    } else if id.as_ref() == Some(&cancel_id) {
                        shell::send(Input::Cancel, cx);
                    } else if id.as_ref() == Some(&quit_id) {
                        cx.quit();
                    }
                })
                .is_err()
            {
                break;
            }
        }
    });
    let mut updates = cx.global::<Services>().output.subscribe();
    let updating_icon = icon.clone();
    let update_task = cx.spawn(async move |cx| {
        let mut previous: Option<TrayState> = None;
        loop {
            let snapshot = updates.borrow_and_update().clone();
            let result = cx.update(|cx| {
                let services = cx.global::<Services>();
                let presentation = state(services, &snapshot);
                let TrayState {
                    indicator,
                    theme,
                    running,
                    pausing,
                    capturing,
                    active,
                    ..
                } = presentation;
                if previous.as_ref() == Some(&presentation) {
                    return;
                }
                if previous
                    .as_ref()
                    .is_none_or(|old| old.indicator != indicator || old.theme != theme)
                    && let Ok(icon) = draw_icon(indicator, theme)
                {
                    #[cfg(target_os = "macos")]
                    let _ = updating_icon.set_icon_with_as_template(Some(icon), true);
                    #[cfg(target_os = "windows")]
                    let _ = updating_icon.set_icon(Some(icon));
                }
                status.set_text(&presentation.description);
                let _ = updating_icon.set_tooltip(Some(format!(
                    "Speakeasy · {}\nHold {} to dictate",
                    presentation.description,
                    speakeasy_platform::SHORTCUT
                )));
                pause.set_text(if pausing {
                    "Pausing…"
                } else if running {
                    "Pause dictation"
                } else {
                    "Resume dictation"
                });
                pause.set_enabled(!pausing && !services.demo);
                toggle.set_text(if capturing {
                    "Finish dictation"
                } else {
                    "Start dictation"
                });
                toggle.set_enabled(running && !pausing && (!active || capturing) && !services.demo);
                cancel.set_enabled(running && !pausing && active && !services.demo);
                previous = Some(presentation);
            });
            if result.is_err() || updates.changed().await.is_err() {
                break;
            }
        }
    });
    cx.set_global(Tray {
        _icon: icon,
        _events: event_task,
        _updates: update_task,
    });
    Ok(())
}

fn state(services: &Services, snapshot: &crate::runtime::Snapshot) -> TrayState {
    let running = services.running() || services.demo;
    let pausing = services.retiring.is_some();
    let capturing = matches!(snapshot.phase, Phase::Starting | Phase::Recording);
    TrayState {
        indicator: status::indicator(snapshot, running, pausing),
        theme: services.config.theme,
        description: status::description(snapshot, running, pausing, services.config.engine),
        running,
        pausing,
        capturing,
        active: capturing || matches!(snapshot.phase, Phase::Stopping | Phase::Processing),
        ready: snapshot.desktop_ready,
    }
}
// The Grille mark: a lamp-lit door slot with its stepped bars cut through.
// Recording turns the slot red. Other states add a cut-jewel badge whose shape
// stays distinct when macOS renders the icon as a template.
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn draw_icon(state: Indicator, theme: Theme) -> anyhow::Result<Icon> {
    Ok(Icon::from_rgba(
        raster(state, theme, cfg!(target_os = "macos")),
        32,
        32,
    )?)
}

const GRILLE: [(f32, f32); 5] = [
    (8.0, 4.0),
    (11.5, 6.0),
    (15.0, 8.0),
    (18.5, 6.0),
    (22.0, 4.0),
];

fn raster(state: Indicator, theme: Theme, template: bool) -> Vec<u8> {
    let palette = theme.palette();
    let slot_color = if state == Indicator::Recording && !template {
        palette.live
    } else {
        palette.lamp
    };
    // Windows draws light badges for the dark taskbar, as system icons do.
    let badge_color = match state {
        Indicator::Recording => palette.live,
        Indicator::Attention => palette.warn,
        _ => 0xe0e0e6,
    };
    let badged = match state {
        Indicator::Ready => false,
        Indicator::Recording => template,
        _ => true,
    };
    let slot_opacity = if state == Indicator::Paused { 0.5 } else { 1.0 };
    let mut rgba = vec![0_u8; 32 * 32 * 4];
    for (index, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((index % 32) as f32 + 0.5, (index / 32) as f32 + 0.5);
        let slot = rounded(x, y, [3.0, 9.0, 29.0, 23.0], 7.0);
        let bars = GRILLE.iter().fold(0.0_f32, |cover, &(left, height)| {
            let bar = [left, 16.0 - height / 2.0, left + 2.0, 16.0 + height / 2.0];
            cover.max(coverage(rounded(x, y, bar, 1.0)))
        });
        let badge = if badged { badge(state, x, y) } else { f32::MAX };
        let cut = if badged {
            coverage(diamond(x, y, 7.8))
        } else {
            0.0
        };
        let layers = [
            // A faint keyline keeps the lamp color legible on light taskbars.
            (
                0x000000,
                if template {
                    0.0
                } else {
                    coverage(slot.min(badge) - 1.0) * 0.45
                },
            ),
            (
                slot_color,
                coverage(slot) * (1.0 - bars) * (1.0 - cut) * slot_opacity,
            ),
            (badge_color, coverage(badge)),
        ];
        let (color, opacity) =
            layers
                .into_iter()
                .fold(([0.0_f32; 3], 0.0_f32), |(below, alpha), (color, top)| {
                    let combined = top + alpha * (1.0 - top);
                    if combined <= 0.0 {
                        return (below, 0.0);
                    }
                    let channel = |i: usize, shift: u32| {
                        let value = ((color >> shift) & 0xff) as f32;
                        (value * top + below[i] * alpha * (1.0 - top)) / combined
                    };
                    ([channel(0, 16), channel(1, 8), channel(2, 0)], combined)
                });
        if opacity > 0.0 {
            let rgb = if template { [0.0; 3] } else { color };
            pixel[..3].copy_from_slice(&rgb.map(|value| value.round() as u8));
            pixel[3] = (opacity * 255.0).round() as u8;
        }
    }
    rgba
}

fn badge(state: Indicator, x: f32, y: f32) -> f32 {
    match state {
        Indicator::Recording => diamond(x, y, 5.0),
        Indicator::Busy => {
            // A three-quarter ring with round caps, open at the upper left.
            let (dx, dy) = (x - 25.0, y - 25.0);
            if dx < 0.0 && dy < 0.0 {
                dx.hypot(y - 20.8).min((x - 20.8).hypot(dy)) - 0.95
            } else {
                (dx.hypot(dy) - 4.2).abs() - 0.95
            }
        }
        Indicator::Paused => rounded(x, y, [21.9, 20.8, 24.2, 29.2], 0.6).min(rounded(
            x,
            y,
            [25.8, 20.8, 28.1, 29.2],
            0.6,
        )),
        Indicator::Attention => {
            rounded(x, y, [23.85, 19.6, 26.15, 26.0], 1.15).min((x - 25.0).hypot(y - 28.7) - 1.3)
        }
        Indicator::Ready => f32::MAX,
    }
}

// Signed distances in icon units; negative inside.
fn rounded(x: f32, y: f32, [left, top, right, bottom]: [f32; 4], radius: f32) -> f32 {
    let qx = (x - (left + right) / 2.0).abs() - ((right - left) / 2.0 - radius);
    let qy = (y - (top + bottom) / 2.0).abs() - ((bottom - top) / 2.0 - radius);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

fn diamond(x: f32, y: f32, size: f32) -> f32 {
    ((x - 25.0).abs() + (y - 25.0).abs() - size) * std::f32::consts::FRAC_1_SQRT_2
}

fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::{Indicator, Theme, raster};

    #[test]
    fn template_states_differ_by_shape_and_recording_changes_color() {
        let states = [
            Indicator::Ready,
            Indicator::Recording,
            Indicator::Busy,
            Indicator::Paused,
            Indicator::Attention,
        ];
        let shapes: Vec<Vec<u8>> = states
            .iter()
            .map(|&state| {
                raster(state, Theme::Jet, true)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| pixel[3])
                    .collect()
            })
            .collect();
        for (index, shape) in shapes.iter().enumerate() {
            assert!(shapes[index + 1..].iter().all(|other| other != shape));
        }
        assert_ne!(
            raster(Indicator::Ready, Theme::Jet, false),
            raster(Indicator::Recording, Theme::Jet, false)
        );
    }
}
