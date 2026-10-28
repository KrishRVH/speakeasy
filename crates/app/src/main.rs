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
mod lowpass;
mod paths;
mod pill;
mod runtime;
mod setup;
mod shell;
mod show;
mod sounds;
mod status;
mod theme;
mod transcript;
mod tray;

use std::{
    borrow::Cow,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

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

impl Invocation {
    fn parse(args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
        let mut args = args.peekable();
        if args.next_if(|arg| arg == "show").is_some() {
            return Ok(Self::Show(show::Command::parse(args)));
        }
        let mut cli = Cli {
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
                "--help" | "-h" => return Ok(Self::Help),
                _ => bail!("Unknown option: {arg}"),
            }
        }
        Ok(Self::App(cli))
    }
}

fn main() -> ExitCode {
    match Invocation::parse(std::env::args().skip(1)) {
        Ok(Invocation::Help) => in_terminal(|| Ok(print_help()?)),
        Ok(Invocation::Show(command)) => in_terminal(|| command.and_then(show::Command::run)),
        Ok(Invocation::App(cli)) => in_dialog(run(cli)),
        Err(error) => in_dialog(Err(error)),
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

/// Runs a command in the terminal that launched it and reports its failure there. Only commands
/// attach a console on Windows: an app sharing the launching console would also receive its Ctrl+C.
fn in_terminal(command: impl FnOnce() -> anyhow::Result<()>) -> ExitCode {
    #[cfg(target_os = "windows")]
    speakeasy_platform::attach_console();
    match command() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "stderr is the last channel left to report a failed command"
            )]
            let _ = writeln!(io::stderr().lock(), "speakeasy: {error:#}");
            ExitCode::FAILURE
        },
    }
}

/// Reports a failed launch in a dialog, since the app has no terminal to report to.
fn in_dialog(launched: anyhow::Result<()>) -> ExitCode {
    match launched {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            speakeasy_platform::show_startup_error(&format!("{error:#}"));
            ExitCode::FAILURE
        },
    }
}

fn print_help() -> io::Result<()> {
    let mut out = io::stdout().lock();
    let commands = if cfg!(target_os = "linux") {
        " | --toggle | --cancel"
    } else {
        ""
    };
    writeln!(
        out,
        "speakeasy [--config PATH] [--demo | --demo-tray{commands}]\nShortcut: {SHORTCUT}"
    )?;
    if cfg!(target_os = "linux") {
        writeln!(
            out,
            "Hold to dictate or double-tap for hands-free. {CANCEL_SHORTCUT} cancels. Desktop-approved bindings may differ.\n--toggle / --cancel control a running app when Desktop bindings is enabled."
        )?;
    } else {
        writeln!(
            out,
            "Hold to dictate; add Space or double-tap for hands-free, or S, W, or A to Show the screen, a window, or an area. {CANCEL_SHORTCUT} cancels."
        )?;
    }
    writeln!(
        out,
        "--demo uses simulated audio without the microphone, global shortcut, model, or clipboard.\n--demo-tray also previews native tray, minimize, close and relaunch behavior."
    )?;
    writeln!(
        out,
        "speakeasy show demo [--out NEW_DIR] writes a scripted Show bundle and prints its summary."
    )
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

    fn parse(args: &[&str]) -> anyhow::Result<Invocation> {
        Invocation::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    fn launch_mode(args: &[&str]) -> anyhow::Result<LaunchMode> {
        let Invocation::App(cli) = parse(args)? else {
            bail!("{args:?} did not launch the app");
        };
        Ok(cli.mode)
    }

    #[test]
    fn demo_never_downgrades_a_tray_preview() -> anyhow::Result<()> {
        assert_eq!(
            launch_mode(&["--demo-tray", "--demo"])?,
            LaunchMode::DemoTray
        );
        assert_eq!(launch_mode(&["--demo"])?, LaunchMode::Demo);
        Ok(())
    }

    #[test]
    fn help_stops_parsing_and_unknown_options_fail() -> anyhow::Result<()> {
        assert!(matches!(parse(&["--help", "--unknown"])?, Invocation::Help));
        assert!(parse(&["--unknown", "--help"]).is_err());
        assert!(parse(&["--config"]).is_err());
        Ok(())
    }

    #[test]
    fn show_is_a_subcommand_only_in_first_position() -> anyhow::Result<()> {
        assert!(matches!(parse(&["show", "demo"])?, Invocation::Show(Ok(_))));
        assert!(matches!(
            parse(&["show", "record"])?,
            Invocation::Show(Err(_))
        ));
        assert!(matches!(parse(&["--help"])?, Invocation::Help));
        assert!(parse(&["--demo", "show"]).is_err());
        Ok(())
    }
}
