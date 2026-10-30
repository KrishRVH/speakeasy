//! Speakeasy's entry point: parses the command line, then launches the native dictation and Show
//! app around its single-owner recording runtime, or runs a command in the terminal.

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
#[cfg(test)]
mod testing;
mod theme;
mod transcript;
mod tray;

use std::{
    borrow::Cow,
    ffi::OsString,
    io::{self, Write as _},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context as _, bail};
use gpui::{App, Application, BorrowAppContext};
use speakeasy_platform::{CANCEL_SHORTCUT, SHORTCUT};
use tokio::sync::watch;

use crate::{
    config::{Config, Startup, ValidConfig},
    gpui_ext::AppUpdate as _,
    instance::{Instance, Request},
    runtime::{Meter, Phase, Snapshot},
    shell::{LaunchMode, Services},
};

// Josefin Sans SemiBold, SIL Open Font License 1.1 (assets/JosefinSans-OFL.txt).
const BRAND_FONT: &str = "Josefin Sans";
const BRAND_FONT_FILE: &[u8] = include_bytes!("../assets/JosefinSans-SemiBold.ttf");

/// What the command line asks for.
enum Invocation {
    Help,
    Version,
    /// Opens the app, from the settings file `--config` names or else the default, which is found
    /// only when the app runs.
    Launch {
        mode: LaunchMode,
        settings: Option<PathBuf>,
    },
    /// Sends a desktop command, offered only on Linux, to the live app holding `settings`.
    Send {
        request: Request,
        settings: Option<PathBuf>,
    },
    Show(show::Command),
}

impl Invocation {
    /// Reads the arguments after the program name, refusing any Speakeasy does not offer.
    fn parse(args: impl Iterator<Item = OsString>) -> anyhow::Result<Self> {
        let mut args = args.peekable();
        if args.next_if(|arg| arg == "show").is_some() {
            return show::Command::parse(args).map(Self::Show);
        }
        let (mut mode, mut request, mut settings) = (LaunchMode::Live, None, None);
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--demo") => {
                    if mode == LaunchMode::Live {
                        mode = LaunchMode::Demo;
                    }
                },
                Some("--demo-tray") => mode = LaunchMode::DemoTray,
                Some("--config") => {
                    let file = args
                        .next()
                        .map(PathBuf::from)
                        .filter(|file| file.file_name().is_some());
                    settings = Some(file.context("--config needs the path of a settings file")?);
                },
                Some("--toggle") => request = Some(Request::Toggle),
                Some("--cancel") => request = Some(Request::Cancel),
                Some("--help" | "-h") => return Ok(Self::Help),
                Some("--version") => return Ok(Self::Version),
                _ => bail!("Unknown option: {}", arg.display()),
            }
        }
        match (request, mode) {
            (None, mode) => Ok(Self::Launch { mode, settings }),
            (Some(request), LaunchMode::Live) if cfg!(target_os = "linux") => {
                Ok(Self::Send { request, settings })
            },
            (Some(_), LaunchMode::Live) => bail!("Desktop commands are available on Linux"),
            (Some(_), LaunchMode::Demo | LaunchMode::DemoTray) => {
                bail!("Desktop commands cannot be combined with a demo")
            },
        }
    }
}

fn main() -> ExitCode {
    let invocation = match Invocation::parse(std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(error) => return Stop::Usage(error).report(terminal()),
    };
    match invocation {
        Invocation::Help => in_terminal(|| Ok(print_help()?)),
        Invocation::Version => in_terminal(|| Ok(print_version()?)),
        Invocation::Launch { mode, settings } => match run(mode, settings) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => Stop::Launch(error).report(terminal()),
        },
        Invocation::Send { request, settings } => {
            in_terminal(|| Instance::send(&settings_file(settings)?, request))
        },
        Invocation::Show(command) => in_terminal(|| command.run()),
    }
}

/// The settings file `settings` names, or else the default, as an absolute path.
fn settings_file(settings: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let file = settings.map_or_else(paths::settings_file, Ok)?;
    Ok(std::path::absolute(file)?)
}

fn run(mode: LaunchMode, settings: Option<PathBuf>) -> anyhow::Result<()> {
    let path = settings_file(settings)?;
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
        load_brand_font(cx);
        let pill = match pill::open(
            updates,
            levels,
            config.prefers_reduced_motion(),
            config.theme,
            cx,
        ) {
            Ok(pill) => pill,
            Err(error) => {
                abandon(error.context("Cannot open pill"), (instance, requests), cx);
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
            abandon(error, (), cx);
            return;
        }
        // Linux always opens Settings so portal permissions and tray availability stay reachable.
        if !configured || !cx.global::<Services>().is_running() || cfg!(target_os = "linux") {
            shell::reveal(cx);
        }
        if !mode.is_resident() {
            cx.on_window_closed(shell::quit_after_settings).detach();
        }
    });
}

/// Ends a launch that cannot go on, once its error has been seen, and keeps `held`, such as the
/// instance lock, until then, so a launch meanwhile finds this one instead of starting another.
/// The dialog waits for a task of its own because its modal loop runs the app's queued tasks, and
/// those cannot borrow the app while `Application::run`'s callback holds it.
fn abandon<Held: 'static>(error: anyhow::Error, held: Held, cx: &App) {
    let message = format!("{error:#}");
    cx.spawn(async move |cx| {
        let _held = held;
        speakeasy_platform::show_startup_error(&message);
        cx.update_if_running(shell::request_quit);
    })
    .detach();
}

/// Runs a command in the terminal that launched it and reports its failure there.
fn in_terminal(command: impl FnOnce() -> anyhow::Result<()>) -> ExitCode {
    speakeasy_platform::attach_stdio();
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

/// Why Speakeasy ends before the app opens.
enum Stop {
    /// The command line asks for something Speakeasy does not offer.
    Usage(anyhow::Error),
    /// The app cannot start, as when it cannot lock its settings directory.
    Launch(anyhow::Error),
}

impl Stop {
    /// Explains the stop on the `terminal` that launched Speakeasy, so a script or agent reads the
    /// reason instead of waiting on a dialog no one sees, and in a dialog only when there is none,
    /// as after a launch from the desktop. A command line exits with the usage-error status 2, and
    /// a failed launch with 1.
    fn report(self, terminal: Option<impl io::Write>) -> ExitCode {
        let (error, hint, status) = match self {
            Self::Usage(error) => (error, "\nspeakeasy --help lists what it accepts.", 2),
            Self::Launch(error) => (error, "", 1),
        };
        match terminal {
            Some(mut terminal) => {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "stderr is the last channel left to report why Speakeasy stops"
                )]
                let _ = writeln!(terminal, "speakeasy: {error:#}{hint}");
            },
            None => speakeasy_platform::show_startup_error(&format!("{error:#}")),
        }
        ExitCode::from(status)
    }
}

/// The standard error of the terminal that launched Speakeasy, if one did.
fn terminal() -> Option<io::Stderr> {
    speakeasy_platform::attach_stdio().then(io::stderr)
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
            "Hold to dictate; add Space or double-tap for hands-free, or S, W, or A to Show the screen, a window, or an area. {CANCEL_SHORTCUT} cancels, a Show only while the shortcut is held."
        )?;
    }
    writeln!(
        out,
        "--demo uses simulated audio without the microphone, global shortcut, model, or clipboard.\n--demo-tray also previews native tray, minimize, close and relaunch behavior."
    )?;
    writeln!(
        out,
        "speakeasy --version prints the version.\nspeakeasy show demo [--out NEW_DIR] writes a scripted Show bundle and prints its summary."
    )
}

fn print_version() -> io::Result<()> {
    writeln!(
        io::stdout().lock(),
        "speakeasy {}",
        env!("CARGO_PKG_VERSION")
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

fn load_brand_font(cx: &App) {
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
        Invocation::parse(args.iter().map(OsString::from))
    }

    fn launch_mode(args: &[&str]) -> anyhow::Result<LaunchMode> {
        let Invocation::Launch { mode, .. } = parse(args)? else {
            bail!("{args:?} did not launch the app");
        };
        Ok(mode)
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
    fn help_and_version_stop_parsing_and_unknown_options_fail() -> anyhow::Result<()> {
        assert!(matches!(parse(&["--help", "--unknown"])?, Invocation::Help));
        assert!(matches!(
            parse(&["--version", "--unknown"])?,
            Invocation::Version
        ));
        assert!(parse(&["--unknown", "--version"]).is_err());
        assert!(parse(&["--config"]).is_err());
        assert!(parse(&["--config", ""]).is_err());
        assert!(parse(&["--config", "/"]).is_err());
        Ok(())
    }

    #[test]
    fn show_is_a_subcommand_only_in_first_position() -> anyhow::Result<()> {
        assert!(matches!(parse(&["show", "demo"])?, Invocation::Show(_)));
        assert!(parse(&["show", "record"]).is_err());
        assert!(parse(&["--demo", "show"]).is_err());
        Ok(())
    }

    #[test]
    fn desktop_commands_need_linux_and_the_live_app() {
        assert_eq!(
            matches!(parse(&["--toggle"]), Ok(Invocation::Send { .. })),
            cfg!(target_os = "linux")
        );
        assert!(parse(&["--cancel", "--demo"]).is_err());
        assert!(parse(&["--demo-tray", "--toggle"]).is_err());
    }

    #[test]
    fn a_stop_is_explained_on_its_terminal_with_its_status() -> anyhow::Result<()> {
        let report = |stop: Stop| {
            let mut terminal = Vec::new();
            let status = stop.report(Some(&mut terminal));
            anyhow::Ok((status, String::from_utf8(terminal)?))
        };
        let usage = parse(&["--verbose"]).err().context("--verbose parsed")?;
        assert_eq!(
            report(Stop::Usage(usage))?,
            (
                ExitCode::from(2),
                "speakeasy: Unknown option: --verbose\nspeakeasy --help lists what it accepts.\n"
                    .into()
            )
        );
        assert_eq!(
            report(Stop::Launch(anyhow::anyhow!("Cannot lock settings")))?,
            (
                ExitCode::FAILURE,
                "speakeasy: Cannot lock settings\n".into()
            )
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn arguments_need_not_be_unicode() -> anyhow::Result<()> {
        use std::os::unix::ffi::OsStringExt as _;
        let path = OsString::from_vec(b"settings-\xff.json".to_vec());
        let config = [OsString::from("--config"), path.clone()];
        let Invocation::Launch { settings, .. } = Invocation::parse(config.into_iter())? else {
            bail!("--config did not launch the app");
        };
        assert_eq!(settings, Some(path.into()));
        assert!(Invocation::parse([OsString::from_vec(vec![0xFF])].into_iter()).is_err());
        Ok(())
    }
}
