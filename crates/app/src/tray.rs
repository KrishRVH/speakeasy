use crate::{
    runtime::Phase,
    shell::{self, Services},
    status::{self, Indicator},
};
use gpui::{App, Global, Task};
use speakeasy_platform::Input;
use tray_icon::{
    Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

pub struct Tray {
    _icon: TrayIcon,
    _events: Task<()>,
    _updates: Task<()>,
}
impl Global for Tray {}

#[derive(PartialEq, Eq)]
struct TrayState {
    indicator: Indicator,
    description: String,
    running: bool,
    pausing: bool,
    capturing: bool,
    active: bool,
}

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
        .with_icon(draw_icon(Indicator::Paused)?)
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
                let running = services.running() || services.demo;
                let pausing = services.retiring.is_some();
                let indicator = status::indicator(&snapshot, running, pausing);
                let description =
                    status::description(&snapshot, running, pausing, services.config.engine);
                let capturing = matches!(snapshot.phase, Phase::Starting | Phase::Recording);
                let active =
                    capturing || matches!(snapshot.phase, Phase::Stopping | Phase::Processing);
                let presentation = TrayState {
                    indicator,
                    description,
                    running,
                    pausing,
                    capturing,
                    active,
                };
                if previous.as_ref() == Some(&presentation) {
                    return;
                }
                if previous
                    .as_ref()
                    .is_none_or(|old| old.indicator != indicator)
                    && let Ok(icon) = draw_icon(indicator)
                {
                    #[cfg(target_os = "macos")]
                    let _ = updating_icon.set_icon_with_as_template(Some(icon), true);
                    #[cfg(target_os = "windows")]
                    let _ = updating_icon.set_icon(Some(icon));
                }
                status.set_text(&presentation.description);
                let _ = updating_icon.set_tooltip(Some(format!(
                    "Speakeasy · {}\nCtrl+Alt+Space",
                    presentation.description
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

// Shape badges remain distinct when macOS renders the waveform as a template.
fn draw_icon(state: Indicator) -> anyhow::Result<Icon> {
    let mut rgba = vec![0_u8; 32 * 32 * 4];
    for y in 0..32_usize {
        for x in 0..32_usize {
            let bar = [(5, 12), (11, 7), (17, 4), (23, 10)]
                .iter()
                .any(|&(left, top)| {
                    (left..left + 3).contains(&x) && (top..28 - top / 2).contains(&y)
                });
            let dx = x as i32 - 25;
            let dy = y as i32 - 25;
            let in_badge = dx * dx + dy * dy <= 49;
            let badge = match state {
                Indicator::Ready => false,
                Indicator::Recording => dx * dx + dy * dy <= 20,
                Indicator::Paused => {
                    (x == 23 || x == 24 || x == 27 || x == 28) && (21..30).contains(&y)
                }
                Indicator::Busy => (16..=30).contains(&(dx * dx + dy * dy)),
                Indicator::Attention => {
                    (24..=26).contains(&x) && ((20..26).contains(&y) || y == 28)
                }
            };
            if badge || (bar && (!in_badge || state == Indicator::Ready)) {
                let color = if badge && !cfg!(target_os = "macos") {
                    match state {
                        Indicator::Recording => [255, 79, 46, 255],
                        Indicator::Attention => [245, 165, 36, 255],
                        _ => [224, 224, 230, 255],
                    }
                } else {
                    [224, 224, 230, 255]
                };
                rgba[(y * 32 + x) * 4..(y * 32 + x + 1) * 4].copy_from_slice(&color);
            }
        }
    }
    Ok(Icon::from_rgba(rgba, 32, 32)?)
}
