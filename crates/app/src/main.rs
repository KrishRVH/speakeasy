//! Native dictation UI and the single-owner recording runtime.

#![forbid(unsafe_code)]

mod gpui_ext;
mod icons;
mod pill;
mod shell;
mod tray;

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, bail, ensure};
use gpui::{App, Application, BorrowAppContext};
use speakeasy_dictation::{
    config::{self, Config},
    instance::{Instance, Request},
    runtime::{Phase, Snapshot},
};
use speakeasy_platform::{CANCEL_SHORTCUT, SHORTCUT};
use tokio::sync::watch;

use crate::shell::{LaunchMode, Services};

// Josefin Sans SemiBold, SIL Open Font License 1.1 (assets/JosefinSans-OFL.txt).
const WORDMARK_FONT: &str = "Josefin Sans";
const WORDMARK_FONT_FILE: &[u8] = include_bytes!("../assets/JosefinSans-SemiBold.ttf");

struct Cli {
    mode: LaunchMode,
    command: Option<Request>,
    path: PathBuf,
}

impl Cli {
    /// Returns `None` when help was requested.
    fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<Option<Self>> {
        let mut cli = Self {
            mode: LaunchMode::Live,
            command: None,
            path: config::default_path(),
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--demo" => {
                    if cli.mode == LaunchMode::Live {
                        cli.mode = LaunchMode::Demo;
                    }
                },
                "--demo-tray" => cli.mode = LaunchMode::DemoTray,
                "--config" => cli.path = args.next().context("--config needs a path")?.into(),
                "--toggle" => cli.command = Some(Request::Toggle),
                "--cancel" => cli.command = Some(Request::Cancel),
                "--help" | "-h" => return Ok(None),
                _ => bail!("Unknown option: {arg}"),
            }
        }
        Ok(Some(cli))
    }
}

enum ConfigState {
    Unconfigured,
    Configured,
    Invalid(String),
}

impl ConfigState {
    fn into_snapshot(self) -> Snapshot {
        match self {
            Self::Invalid(message) => Snapshot {
                message,
                phase: Phase::Error,
                ..Snapshot::default()
            },
            // Applying a configured app's settings replaces this placeholder before it shows.
            Self::Unconfigured | Self::Configured => Snapshot {
                message: "Speakeasy is not set up yet.".into(),
                ..Snapshot::default()
            },
        }
    }
}

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    // The speech helper is this executable re-run by the session owner; it never starts AppKit.
    if arguments.next().as_deref() == Some(std::ffi::OsStr::new(speakeasy_dictation::HELPER_FLAG)) {
        speakeasy_dictation::run_helper(arguments);
    }
    if let Err(error) = run() {
        speakeasy_platform::show_error(&format!("{error:#}"));
    }
}

fn run() -> anyhow::Result<()> {
    let Some(Cli {
        mode,
        command,
        path,
    }) = Cli::parse(std::env::args().skip(1))?
    else {
        print_help();
        return Ok(());
    };
    let path = std::path::absolute(path)?;
    if let Some(command) = command {
        ensure!(
            !mode.is_demo(),
            "Desktop commands cannot be combined with a demo"
        );
        return Instance::send(&path, command);
    }
    let acquired = if mode.is_resident() {
        let Some(acquired) = Instance::acquire(&path)? else {
            return Ok(());
        };
        Some(acquired)
    } else {
        None
    };
    launch(path, mode, acquired);
    Ok(())
}

fn launch(
    path: PathBuf,
    mode: LaunchMode,
    acquired: Option<(Instance, async_channel::Receiver<Request>)>,
) {
    let (instance, requests) = acquired.unzip();
    let (config, state) = load_config(&path, mode);
    let configured = matches!(state, ConfigState::Configured);
    let (output, updates) = watch::channel(state.into_snapshot());
    let application = Application::new().with_assets(icons::Icons);
    application.on_reopen(shell::reveal);
    application.run(move |cx| {
        load_wordmark_font(cx);
        let pill = match pill::open(updates, config.prefers_reduced_motion(), config.theme, cx) {
            Ok(pill) => pill,
            Err(error) => {
                speakeasy_platform::show_error(&format!("Cannot open pill: {error:#}"));
                cx.quit();
                return;
            },
        };
        cx.set_global(Services::new(
            path,
            config.clone(),
            output,
            pill,
            instance,
            mode,
        ));
        if let Some(requests) = requests {
            cx.update_global::<Services, _>(|services, cx| {
                services.listen_for_requests(requests, cx);
            });
        }
        cx.on_app_quit(release_services).detach();
        if configured
            && let Err(error) =
                cx.update_global::<Services, _>(|services, cx| services.apply(config, cx))
        {
            cx.global::<Services>().publish_error(&error);
        }
        if mode.is_resident()
            && let Err(error) = tray::install(cx)
        {
            speakeasy_platform::show_error(&format!("{error:#}"));
            shell::request_quit(cx);
            return;
        }
        if !configured || !cx.global::<Services>().running() {
            shell::reveal(cx);
        }
        if !mode.is_resident() {
            cx.on_window_closed(shell::request_quit).detach();
        }
    });
}

fn print_help() {
    println!(
        "speakeasy [--config PATH] [--demo | --demo-tray | --toggle | --cancel]\nShortcut: {SHORTCUT}"
    );
    println!(
        "Hold to dictate; add Space or double-tap for hands-free. {CANCEL_SHORTCUT} cancels.\n--toggle / --cancel start, finish or discard dictation in the running app, for scripts."
    );
    println!(
        "--demo uses simulated audio without microphone, keyboard monitoring, or clipboard access.\n--demo-tray also previews the menu bar item, closing Settings, and relaunching."
    );
}

/// Demos and first launches start unconfigured from defaults; saved settings must parse and
/// validate.
fn load_config(path: &Path, mode: LaunchMode) -> (Config, ConfigState) {
    if mode.is_demo() || !path.exists() {
        return (Config::default(), ConfigState::Unconfigured);
    }
    let mut config = match Config::read(path) {
        Ok(config) => config,
        Err(error) => return (Config::default(), ConfigState::Invalid(error.to_string())),
    };
    if config.engine_executable.as_os_str().is_empty() {
        return (config, ConfigState::Unconfigured);
    }
    let state = match config.validate(path) {
        Ok(()) => ConfigState::Configured,
        Err(error) => ConfigState::Invalid(error.to_string()),
    };
    (config, state)
}

fn load_wordmark_font(cx: &App) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "An unavailable embedded brand face falls back to the system font"
    )]
    let _ = cx
        .text_system()
        .add_fonts(vec![Cow::Borrowed(WORDMARK_FONT_FILE)]);
}

fn release_services(cx: &mut App) -> impl Future<Output = ()> + use<> {
    if cx.has_global::<tray::Tray>() {
        drop(cx.remove_global::<tray::Tray>());
    }
    let instance = cx.remove_global::<Services>().into_instance();
    // GPUI closes every window, flushing Settings' final save, before polling this future, so a
    // relaunch cannot race that save for the instance lock.
    async move { drop(instance) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Option<Cli>> {
        Cli::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn demo_never_downgrades_a_tray_preview() -> anyhow::Result<()> {
        let cli = parse(&["--demo-tray", "--demo"])?.context("Help was not requested")?;
        assert_eq!(cli.mode, LaunchMode::DemoTray);
        let cli = parse(&["--demo"])?.context("Help was not requested")?;
        assert_eq!(cli.mode, LaunchMode::Demo);
        Ok(())
    }

    #[test]
    fn help_stops_parsing_and_unknown_options_fail() -> anyhow::Result<()> {
        assert!(parse(&["--help", "--unknown"])?.is_none());
        assert!(parse(&["--unknown", "--help"]).is_err());
        assert!(parse(&["--config"]).is_err());
        Ok(())
    }
}
