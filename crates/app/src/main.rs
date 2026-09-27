#![forbid(unsafe_code)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod audio;
mod config;
mod instance;
mod local_speech;
mod pill;
mod ports;
mod runtime;
mod shell;
mod status;
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod tray;
use anyhow::{Context as _, bail};
use gpui::*;
use runtime::Snapshot;

fn main() {
    if let Err(error) = run() {
        speakeasy_platform::show_error(&format!("{error:#}"));
    }
}

fn run() -> anyhow::Result<()> {
    let mut demo = false;
    let mut demo_tray = false;
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
            "--help" | "-h" => {
                println!(
                    "speakeasy [--config PATH] [--demo | --demo-tray]\nCtrl+Alt+Space: hold to dictate; double tap for hands-free. Escape cancels.\n--demo uses simulated audio without microphone, hook, or clipboard access.\n--demo-tray also previews native tray, minimize, close and relaunch behavior."
                );
                return Ok(());
            }
            _ => bail!("Unknown option: {arg}"),
        }
    }
    if demo_tray && !cfg!(any(target_os = "windows", target_os = "macos")) {
        bail!("Tray preview requires Windows or macOS. Use --demo for the motion preview.");
    }
    if path.is_relative() {
        path = std::env::current_dir()?.join(path);
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
        Ok(config) => (
            config,
            "Choose your local engine and model, then enable dictation.".to_owned(),
            false,
        ),
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
            demo_tray,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            visibility: None,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            tray_hint_seen: false,
            _reopen: None,
            _instance: instance,
        });
        if let Some(reopen) = reopen {
            let task = cx.spawn(async move |cx| {
                while reopen.recv().await.is_ok() {
                    if cx.update(shell::reveal).is_err() {
                        break;
                    }
                }
            });
            cx.global_mut::<shell::Services>()._reopen = Some(task);
        }
        cx.on_app_quit(|cx| {
            #[cfg(any(target_os = "windows", target_os = "macos"))]
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
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if (!demo || demo_tray)
            && let Err(error) = tray::install(cx)
        {
            speakeasy_platform::show_error(&error.to_string());
            cx.quit();
            return;
        }
        if demo || !configured || !cx.global::<shell::Services>().running() {
            shell::reveal(cx);
        }
        if demo && !demo_tray {
            cx.on_window_closed(|cx| cx.quit()).detach();
        }
    });
    Ok(())
}
