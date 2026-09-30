#![forbid(unsafe_code)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod audio;
mod config;
mod instance;
mod local_speech;
mod pill;
mod ports;
mod runtime;
mod setup;
mod shell;
mod status;
mod theme;
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
mod tray;
use anyhow::{Context as _, bail};
use gpui::*;
use runtime::Snapshot;

// Josefin Sans SemiBold, SIL Open Font License 1.1 (assets/JosefinSans-OFL.txt).
const WORDMARK_FONT: &str = "Josefin Sans";

fn main() {
    if let Err(error) = run() {
        speakeasy_platform::show_error(&format!("{error:#}"));
    }
}

fn run() -> anyhow::Result<()> {
    let mut demo = false;
    let mut demo_tray = false;
    let mut command = None;
    let mut path = config::default_path();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--demo" => demo = true,
            "--demo-tray" => {
                demo = true;
                demo_tray = true;
            }
            "--config" => path = args.next().context("--config needs a path")?.into(),
            "--toggle" => command = Some(instance::Request::Toggle),
            "--cancel" => command = Some(instance::Request::Cancel),
            "--help" | "-h" => {
                let gestures = if cfg!(target_os = "linux") {
                    "Ctrl+Super+Space: hold to dictate or double-tap for hands-free. Ctrl+Super+Escape cancels. Desktop-approved bindings may differ.\n--toggle / --cancel control a running app when Desktop bindings is enabled."
                } else {
                    "Hold the dictation shortcut; add Space or double-tap for hands-free. Escape cancels."
                };
                println!(
                    "speakeasy [--config PATH] [--demo | --demo-tray]\nShortcut: {}\n{gestures}\n--demo uses simulated audio without microphone, hook, or clipboard access.\n--demo-tray also previews native tray, minimize, close and relaunch behavior.",
                    speakeasy_platform::SHORTCUT
                );
                return Ok(());
            }
            _ => bail!("Unknown option: {arg}"),
        }
    }
    if demo_tray
        && !cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        ))
    {
        bail!("Tray preview requires Windows, macOS or Linux. Use --demo for the motion preview.");
    }
    if path.is_relative() {
        path = std::env::current_dir()?.join(path);
    }
    if let Some(command) = command {
        anyhow::ensure!(!demo, "Desktop commands cannot be combined with a demo");
        anyhow::ensure!(
            cfg!(target_os = "linux"),
            "Desktop commands are available on Linux"
        );
        return instance::Instance::command(&path, command);
    }
    let (instance, reopen) = if demo && !demo_tray {
        (None, None)
    } else {
        let Some((instance, reopen)) = instance::Instance::acquire(&path)? else {
            return Ok(());
        };
        (Some(instance), Some(reopen))
    };
    let loaded = if demo || !path.exists() {
        Ok(config::Config::default())
    } else {
        config::Config::read(&path)
    };
    let (mut config, mut message, mut invalid) = match loaded {
        Ok(config) => (config, "Speakeasy is not set up yet.".to_owned(), false),
        Err(error) => (config::Config::default(), error.to_string(), true),
    };
    let configured = if !config.engine_executable.as_os_str().is_empty() {
        match config.validate(&path) {
            Ok(()) => true,
            Err(error) => {
                message = error.to_string();
                invalid = true;
                false
            }
        }
    } else {
        false
    };
    let (output, updates) = tokio::sync::watch::channel(Snapshot {
        message,
        phase: if invalid {
            runtime::Phase::Error
        } else {
            runtime::Phase::Idle
        },
        ..Snapshot::default()
    });
    let application = Application::new();
    application.on_reopen(shell::reveal);
    application.run(move |cx| {
        // Without the brand face, the wordmark falls back to the system font.
        let _ = cx
            .text_system()
            .add_fonts(vec![std::borrow::Cow::Borrowed(include_bytes!(
                "../assets/JosefinSans-SemiBold.ttf"
            ))]);
        let reduced = config.reduced_motion || speakeasy_platform::reduced_motion();
        let pill = match pill::open(updates, reduced, config.theme, cx) {
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
            demo_tray,
            #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
            visibility: None,
            #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
            tray_hint_seen: false,
            _reopen: None,
            _instance: instance,
        });
        if let Some(reopen) = reopen {
            let task = cx.spawn(async move |cx| {
                while let Ok(request) = reopen.recv().await {
                    if cx
                        .update(|cx| match request {
                            instance::Request::Reveal => shell::reveal(cx),
                            instance::Request::Toggle | instance::Request::Cancel => {
                                #[cfg(target_os = "linux")]
                                if cx
                                    .global::<shell::Services>()
                                    .config
                                    .linux
                                    .external_shortcut
                                {
                                    shell::send(
                                        if request == instance::Request::Toggle {
                                            speakeasy_platform::Input::Toggle
                                        } else {
                                            speakeasy_platform::Input::Cancel
                                        },
                                        cx,
                                    );
                                }
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
            cx.global_mut::<shell::Services>()._reopen = Some(task);
        }
        cx.on_app_quit(|cx| {
            #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
            if cx.has_global::<tray::Tray>() {
                drop(cx.remove_global::<tray::Tray>());
            }
            drop(cx.remove_global::<shell::Services>());
            async {}
        })
        .detach();
        if !demo
            && configured
            && let Err(error) =
                cx.update_global::<shell::Services, _>(|services, cx| services.apply(config, cx))
        {
            cx.global::<shell::Services>().output.send_modify(|s| {
                s.phase = runtime::Phase::Error;
                s.message = error.to_string();
            });
        }
        #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
        if (!demo || demo_tray)
            && let Err(error) = tray::install(cx)
        {
            speakeasy_platform::show_error(&error.to_string());
            cx.quit();
            return;
        }
        if demo
            || !configured
            || !cx.global::<shell::Services>().running()
            || cfg!(target_os = "linux")
        {
            shell::reveal(cx);
        }
        if demo && !demo_tray {
            cx.on_window_closed(|cx| cx.quit()).detach();
        }
    });
    Ok(())
}
