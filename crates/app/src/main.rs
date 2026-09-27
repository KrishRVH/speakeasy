#![forbid(unsafe_code)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod audio;
mod config;
mod local_speech;
mod pill;
mod ports;
mod runtime;
mod shell;
use anyhow::{Context as _, bail};
use gpui::*;
use runtime::Snapshot;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use speakeasy_platform::Input;

fn main() {
    if let Err(error) = run() {
        speakeasy_platform::show_error(&format!("{error:#}"));
    }
}

fn run() -> anyhow::Result<()> {
    let mut demo = false;
    let mut path = config::default_path();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--demo" => demo = true,
            "--config" => path = args.next().context("--config needs a path")?.into(),
            "--help" | "-h" => {
                println!(
                    "speakeasy [--config PATH] [--demo]\nCtrl+Alt+Space: hold to dictate; double tap for hands-free. Escape cancels.\n--demo uses simulated audio without microphone, hook, or clipboard access."
                );
                return Ok(());
            }
            _ => bail!("Unknown option: {arg}"),
        }
    }
    if path.is_relative() {
        path = std::env::current_dir()?.join(path);
    }
    let _instance = if !demo {
        let directory = path.parent().context("Settings path has no directory")?;
        std::fs::create_dir_all(directory)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("instance.lock"))?;
        lock.try_lock()
            .context("Speakeasy is already running. Open it from the tray.")?;
        Some(lock)
    } else {
        None
    };
    let loaded = if demo || !path.exists() {
        Ok(config::Config::default())
    } else {
        config::Config::read(&path)
    };
    let (mut config, mut message) = match loaded {
        Ok(config) => (
            config,
            "Choose your local engine and model, then enable dictation.".to_owned(),
        ),
        Err(error) => (config::Config::default(), error.to_string()),
    };
    let configured = if !config.engine_executable.as_os_str().is_empty() {
        match config.validate(&path) {
            Ok(()) => true,
            Err(error) => {
                message = error.to_string();
                false
            }
        }
    } else {
        false
    };
    let (output, updates) = tokio::sync::watch::channel(Snapshot {
        message,
        ..Snapshot::default()
    });
    Application::new().run(move |cx| {
        let reduced = config.reduced_motion || speakeasy_platform::reduced_motion();
        let pill = match pill::open(updates, reduced, cx) {
            Ok(pill) => pill,
            Err(error) => {
                speakeasy_platform::show_error(&format!("Cannot open pill: {error}"));
                cx.quit();
                return;
            }
        };
        cx.set_global(shell::Services {
            monitor: None,
            runtime: None,
            retiring: None,
            retirement: None,
            pending: None,
            path,
            config: config.clone(),
            output,
            pill,
            window: None,
            demo,
        });
        cx.on_app_quit(|cx| {
            drop(cx.remove_global::<shell::Services>());
            async {}
        })
        .detach();
        if !demo
            && configured
            && let Err(error) =
                cx.update_global::<shell::Services, _>(|services, cx| services.apply(config, cx))
        {
            cx.global::<shell::Services>()
                .output
                .send_modify(|s| s.message = error.to_string());
        }
        if let Err(error) = shell::open(cx) {
            speakeasy_platform::show_error(&error.to_string());
            cx.quit();
            return;
        }
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if !demo {
            match tray(cx) {
                Ok(tray) => cx.set_global(Tray { _icon: tray }),
                Err(error) => {
                    speakeasy_platform::show_error(&error.to_string());
                    cx.quit();
                }
            }
        }
        if demo {
            cx.on_window_closed(|cx| cx.quit()).detach();
        }
    });
    Ok(())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
struct Tray {
    _icon: tray_icon::TrayIcon,
}
#[cfg(any(target_os = "windows", target_os = "macos"))]
impl Global for Tray {}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn tray(cx: &mut App) -> anyhow::Result<tray_icon::TrayIcon> {
    use tray_icon::{
        Icon, TrayIconBuilder,
        menu::{Menu, MenuEvent, MenuItem},
    };
    let menu = Menu::new();
    let show = MenuItem::new("Open Speakeasy", true, None);
    let toggle = MenuItem::new("Start / finish dictation", true, None);
    let cancel = MenuItem::new("Cancel", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[&show, &toggle, &cancel, &quit])?;
    let show_id = show.id().clone();
    let quit_id = quit.id().clone();
    let toggle_id = toggle.id().clone();
    let (tx, rx) = async_channel::bounded(16);
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = tx.try_send(event);
    }));
    cx.spawn(async move |cx| {
        while let Ok(event) = rx.recv().await {
            if event.id == show_id {
                let _ = cx.update(|cx| {
                    if let Err(error) = shell::open(cx) {
                        speakeasy_platform::show_error(&error.to_string());
                    }
                });
                continue;
            }
            let action = if event.id == quit_id {
                Input::Quit
            } else if event.id == toggle_id {
                Input::Toggle
            } else {
                Input::Cancel
            };
            let _ = cx.update(|cx| shell::send(action, cx));
            if matches!(action, Input::Quit) {
                let _ = cx.update(|cx| cx.quit());
                break;
            }
        }
    })
    .detach();
    let mut rgba = vec![0_u8; 32 * 32 * 4];
    for y in 0_i32..32 {
        for x in 0_i32..32 {
            if (x - 16).pow(2) + (y - 16).pow(2) < 100 {
                let offset = (y * 32 + x) as usize * 4;
                rgba[offset..offset + 4].copy_from_slice(&[224, 224, 230, 255]);
            }
        }
    }
    Ok(TrayIconBuilder::new()
        .with_tooltip("Speakeasy · Ctrl+Alt+Space")
        .with_icon_as_template(true)
        .with_icon(Icon::from_rgba(rgba, 32, 32)?)
        .with_menu(Box::new(menu))
        .build()?)
}
