//! Speakeasy's entry point: parses the command line, then launches the native dictation and Show
//! app around its single-owner recording runtime, or runs a `show` command in the terminal.

#![forbid(unsafe_code)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod audio;
mod child;
mod config;
mod gpui_ext;
mod http;
mod icons;
mod instance;
mod local_speech;
mod paths;
mod pill;
mod runtime;
mod setup;
mod shell;
mod show;
mod status;
mod theme;
mod transcript;
mod tray;

use std::{borrow::Cow, path::PathBuf, process::ExitCode};

use anyhow::{Context as _, bail, ensure};
use gpui::{App, Application, BorrowAppContext};
use speakeasy_platform::{CANCEL_SHORTCUT, SHORTCUT};
use tokio::sync::watch;

use crate::{
    config::{Config, Startup, ValidConfig},
    instance::{Instance, Request},
    runtime::{Meter, Phase, Snapshot},
    shell::{LaunchMode, Services},
};

// Josefin Sans SemiBold, SIL Open Font License 1.1 (assets/JosefinSans-OFL.txt), draws the
// wordmark and the badges on Show keyframes.
const WORDMARK_FONT: &str = "Josefin Sans";
const BRAND_FONT_FILE: &[u8] = include_bytes!("../assets/JosefinSans-SemiBold.ttf");

struct Cli {
    mode: LaunchMode,
    command: Option<Request>,
    /// The settings file `--config` names; otherwise the default, found only when the app runs.
    path: Option<PathBuf>,
}

/// What the command line asks for.
enum Invocation {
    Help,
    App(Cli),
    /// A `show` subcommand, or why its arguments were not understood.
    Show(anyhow::Result<show::Command>),
}

impl Cli {
    fn parse(args: impl Iterator<Item = String>) -> anyhow::Result<Invocation> {
        let mut args = args.peekable();
        if args.next_if(|arg| arg == "show").is_some() {
            return Ok(Invocation::Show(show::Command::parse(args)));
        }
        Ok(Self::parse_app(args)?.map_or(Invocation::Help, Invocation::App))
    }

    /// Returns `None` when help was requested.
    fn parse_app(mut args: impl Iterator<Item = String>) -> anyhow::Result<Option<Self>> {
        let mut cli = Self {
            mode: LaunchMode::Live,
            command: None,
            path: None,
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--demo" => {
                    if cli.mode == LaunchMode::Live {
                        cli.mode = LaunchMode::Demo;
                    }
                },
                "--demo-tray" => cli.mode = LaunchMode::DemoTray,
                "--config" => {
                    cli.path = Some(args.next().context("--config needs a path")?.into());
                },
                "--toggle" => cli.command = Some(Request::Toggle),
                "--cancel" => cli.command = Some(Request::Cancel),
                "--help" | "-h" => return Ok(None),
                _ => bail!("Unknown option: {arg}"),
            }
        }
        Ok(Some(cli))
    }
}

fn main() -> ExitCode {
    let invocation = Cli::parse(std::env::args().skip(1));
    if matches!(invocation, Ok(Invocation::Show(_) | Invocation::Help)) {
        attach_console();
    }
    match invocation {
        Ok(Invocation::Help) => {
            print_help();
            ExitCode::SUCCESS
        },
        // Commands report to the terminal that ran them, not in a dialog.
        Ok(Invocation::Show(command)) => match command.and_then(show::Command::run) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("speakeasy: {error:#}");
                ExitCode::FAILURE
            },
        },
        Ok(Invocation::App(cli)) => match run(cli) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                speakeasy_platform::show_startup_error(&format!("{error:#}"));
                ExitCode::FAILURE
            },
        },
        Err(error) => {
            speakeasy_platform::show_startup_error(&format!("{error:#}"));
            ExitCode::FAILURE
        },
    }
}

fn run(
    Cli {
        mode,
        command,
        path,
    }: Cli,
) -> anyhow::Result<()> {
    let path = std::path::absolute(path.map_or_else(paths::settings_file, Ok)?)?;
    if let Some(command) = command {
        ensure!(
            !mode.is_demo(),
            "Desktop commands cannot be combined with a demo"
        );
        ensure!(
            cfg!(target_os = "linux"),
            "Desktop commands are available on Linux"
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
    // A demo is simulated, so it never reads the user's settings.
    let (config, startup) = if mode.is_demo() {
        (Config::first_launch(), Startup::Unconfigured)
    } else {
        Config::load(&path)
    };
    let (valid, snapshot) = split_startup(startup);
    let configured = valid.is_some();
    let (output, updates) = watch::channel(snapshot);
    let (meter, levels) = watch::channel(Meter::default());
    let application = Application::new().with_assets(icons::Icons);
    application.on_reopen(shell::reveal);
    application.run(move |cx| {
        load_wordmark_font(cx);
        let pill = match pill::open(
            updates,
            levels,
            config.prefers_reduced_motion(),
            config.theme,
            cx,
        ) {
            Ok(pill) => pill,
            Err(error) => {
                speakeasy_platform::show_startup_error(&format!("Cannot open pill: {error:#}"));
                cx.quit();
                return;
            },
        };
        let (canvas, ink) = watch::channel(show::Canvas::default());
        show::attend_overlay(
            ink,
            output.subscribe(),
            |error, cx| cx.global::<Services>().report(error),
            cx,
        );
        cx.set_global(Services::new(
            path, config, output, meter, canvas, pill, instance, mode,
        ));
        if let Some(requests) = requests {
            cx.update_global::<Services, _>(|services, cx| {
                services.listen_for_requests(requests, cx);
            });
        }
        cx.on_app_quit(release_services).detach();
        if mode.is_resident() {
            show::attend_retention(|error, cx| cx.global::<Services>().report(error), cx);
        }
        if let Some(valid) = valid
            && let Err(error) =
                cx.update_global::<Services, _>(|services, cx| services.apply(valid, cx))
        {
            cx.global::<Services>().publish_error(&error);
        }
        if mode.is_resident()
            && let Err(error) = tray::install(cx)
        {
            speakeasy_platform::show_startup_error(&format!("{error:#}"));
            shell::request_quit(cx);
            return;
        }
        // Linux always opens Settings so portal permissions and tray availability stay reachable.
        if !configured || !cx.global::<Services>().running() || cfg!(target_os = "linux") {
            shell::reveal(cx);
        }
        if !mode.is_resident() {
            cx.on_window_closed(shell::quit_after_settings).detach();
        }
    });
}

/// Gives command-line output a terminal on Windows. Only commands attach: an app sharing the
/// launching console would also receive its Ctrl+C.
fn attach_console() {
    #[cfg(target_os = "windows")]
    speakeasy_platform::attach_console();
}

fn print_help() {
    let commands = if cfg!(target_os = "linux") {
        " | --toggle | --cancel"
    } else {
        ""
    };
    println!("speakeasy [--config PATH] [--demo | --demo-tray{commands}]\nShortcut: {SHORTCUT}");
    if cfg!(target_os = "linux") {
        println!(
            "Hold to dictate or double-tap for hands-free. {CANCEL_SHORTCUT} cancels. Desktop-approved bindings may differ.\n--toggle / --cancel control a running app when Desktop bindings is enabled."
        );
    } else {
        println!(
            "Hold to dictate; add Space or double-tap for hands-free, or S to record a Show. {CANCEL_SHORTCUT} cancels."
        );
    }
    println!(
        "--demo uses simulated audio without the microphone, global shortcut, model, or clipboard.\n--demo-tray also previews native tray, minimize, close and relaunch behavior."
    );
    println!(
        "speakeasy show demo [--out NEW_DIR] writes a scripted Show bundle and prints its summary."
    );
}

/// Splits the launch state into the settings to apply, if any, and the first snapshot.
fn split_startup(startup: Startup) -> (Option<ValidConfig>, Snapshot) {
    // Applying a configured app's settings replaces this placeholder before it shows.
    let placeholder = || Snapshot {
        message: "Speakeasy is not set up yet.".into(),
        ..Snapshot::default()
    };
    match startup {
        Startup::Invalid(message) => (
            None,
            Snapshot {
                message,
                phase: Phase::Error,
                ..Snapshot::default()
            },
        ),
        Startup::Unconfigured => (None, placeholder()),
        Startup::Configured(valid) => (Some(valid), placeholder()),
    }
}

fn load_wordmark_font(cx: &App) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "An unavailable embedded brand face falls back to the system font"
    )]
    let _ = cx
        .text_system()
        .add_fonts(vec![Cow::Borrowed(BRAND_FONT_FILE)]);
}

/// Drops the tray before Services, since its tasks reach Services, and releases the instance lock
/// last.
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
        Cli::parse_app(args.iter().map(|arg| (*arg).to_owned()))
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

    #[test]
    fn show_is_a_subcommand_only_in_first_position() -> anyhow::Result<()> {
        let invoke = |args: &[&str]| Cli::parse(args.iter().map(|arg| (*arg).to_owned()));
        assert!(matches!(
            invoke(&["show", "demo"])?,
            Invocation::Show(Ok(_))
        ));
        assert!(matches!(
            invoke(&["show", "record"])?,
            Invocation::Show(Err(_))
        ));
        assert!(matches!(invoke(&["--help"])?, Invocation::Help));
        assert!(invoke(&["--demo", "show"]).is_err());
        Ok(())
    }
}
