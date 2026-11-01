#![forbid(unsafe_code)]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod audio;
mod child;
mod config;
mod control;
mod gpui_ext;
mod http;
mod icons;
mod instance;
mod local_speech;
mod lowpass;
mod models;
mod paths;
mod pill;
mod recognition_terms;
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
mod updates;

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
    instance::{Event, Instance, Request},
    runtime::{Meter, Phase, Report, Snapshot},
    shell::{LaunchMode, Services},
};

const BRAND_FONT: &str = "Josefin Sans";
const BRAND_FONT_FILE: &[u8] = include_bytes!("../assets/JosefinSans-SemiBold.ttf");

enum Invocation {
    Help,
    Version,
    Launch {
        mode: LaunchMode,
        settings: Option<PathBuf>,
    },
    Send {
        request: Request,
        settings: Option<PathBuf>,
    },
    Show(show::Command),
    Control(control::Invocation),
}

impl Invocation {
    fn parse(args: impl Iterator<Item = OsString>) -> anyhow::Result<Self> {
        let mut args = args.peekable();
        if args.next_if(|arg| arg == "show").is_some() {
            return show::Command::parse(args).map(Self::Show);
        }
        if args
            .peek()
            .is_some_and(|arg| control::Invocation::recognizes(arg))
        {
            return control::Invocation::parse(args).map(Self::Control);
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
    #[cfg(all(feature = "native-acceptance", target_os = "macos"))]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--check-owned-quit" || arg == "--send-owned-quit")
    {
        return in_terminal(shell::native_quit_check);
    }
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
        Invocation::Control(invocation) => {
            let path = match settings_file(invocation.settings) {
                Ok(path) => path,
                Err(error) => return in_terminal(|| Err(error)),
            };
            let reply = Instance::control(&path, invocation.command);
            let code = reply.exit_code();
            let written = in_terminal(|| reply.write(&mut io::stdout().lock()));
            if written != ExitCode::SUCCESS && reply.result != control::Disposition::Refused {
                ExitCode::from(2)
            } else {
                ExitCode::from(code)
            }
        },
    }
}

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
    launch(path, mode, acquired)
}

#[cfg_attr(
    not(target_os = "windows"),
    expect(
        clippy::unnecessary_wraps,
        reason = "Windows reports installer handoff failures after the UI exits"
    )
)]
fn launch(
    path: PathBuf,
    mode: LaunchMode,
    acquired: Option<(Instance, async_channel::Receiver<Event>)>,
) -> anyhow::Result<()> {
    let (instance, requests) = acquired.unzip();
    let default_settings = settings_file(None).is_ok_and(|default| default == path);

    let (config, startup) = if mode.is_demo() {
        (Config::default(), Startup::Unconfigured)
    } else {
        Config::load(&path)
    };
    #[cfg(target_os = "windows")]
    let sign_in = sign_in_launch(mode, default_settings, &startup, &config);
    let (valid, snapshot) = split_startup(startup);
    let configured = valid.is_some();
    let (output, updates) = watch::channel(snapshot);
    let (meter, levels) = watch::channel(Meter::default());
    let application = Application::new().with_assets(icons::Icons);
    application.on_reopen(shell::reveal);
    #[cfg(target_os = "macos")]
    application.on_quit_requested(shell::request_quit);
    #[cfg(target_os = "windows")]
    let (handoff, ready) = std::sync::mpsc::channel();
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
            path,
            default_settings,
            config,
            output,
            meter,
            canvas,
            pill,
            instance,
            mode,
        ));
        if let Some(requests) = requests {
            cx.update_global::<Services, _>(|services, cx| {
                services.listen_for_requests(requests, cx);
            });
        }
        #[cfg(target_os = "windows")]
        cx.on_app_quit(move |cx| release_services(cx, handoff.clone()))
            .detach();
        #[cfg(not(target_os = "windows"))]
        cx.on_app_quit(release_services).detach();
        if mode == LaunchMode::Live {
            show::attend_retention(
                |error, cx| {
                    cx.global::<Services>()
                        .report(Report::Recurring(error.to_string()));
                },
                cx,
            );
        }
        let applied = valid.map_or(Ok(()), |valid| {
            cx.update_global::<Services, _>(|services, cx| services.apply(valid, cx))
        });
        if let Err(error) = &applied {
            cx.global::<Services>().publish_error(error);
        }

        #[cfg(target_os = "windows")]
        if let Some(on) = sign_in
            && let Err(error) = speakeasy_platform::start_at_login(on)
            && applied.is_ok()
        {
            cx.global::<Services>().report(&error);
        }
        if mode.is_resident()
            && let Err(error) = tray::install(cx)
        {
            abandon(error, (), cx);
            return;
        }

        if !configured || !cx.global::<Services>().is_running() || cfg!(target_os = "linux") {
            shell::reveal(cx);
        }
        if !mode.is_resident() {
            cx.on_window_closed(shell::quit_after_settings).detach();
        }
        if mode == LaunchMode::Demo {
            shell::reveal_shows(cx);
        }
    });
    #[cfg(target_os = "windows")]
    if let Ok(installer) = ready.try_recv() {
        installer.launch()?;
    }
    Ok(())
}

fn abandon<Held: 'static>(error: anyhow::Error, held: Held, cx: &App) {
    let message = format!("{error:#}");
    cx.spawn(async move |cx| {
        let _held = held;
        speakeasy_platform::show_startup_error(&message);
        cx.update_if_running(shell::request_quit);
    })
    .detach();
}

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

enum Stop {
    Usage(anyhow::Error),
    Launch(anyhow::Error),
}

impl Stop {
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
            "Hold to dictate; add Space or double-tap for hands-free, or S, W, or A to Show the screen, a window, or an area. Add V to paste the last dictation. {CANCEL_SHORTCUT} cancels, a Show only while the shortcut is held."
        )?;
    }
    writeln!(
        out,
        "--demo uses simulated audio without the microphone, global shortcut, model, or clipboard.\n--demo-tray also previews native tray, minimize, close and relaunch behavior."
    )?;
    writeln!(
        out,
        "speakeasy status [--config PATH]\nspeakeasy dictation start [--style STYLE] [--config PATH]\nspeakeasy dictation stop|cancel TOKEN [--config PATH]\nspeakeasy paste-last [--config PATH]\nspeakeasy style STYLE [--config PATH]\nspeakeasy preset NAME [--config PATH]\n\nControl commands return JSON: exit 0 acknowledged, 1 refused, 2 unknown or not running.\nStart returns a session token; stop and cancel require that exact token.\nStyles: prose, verbatim, camel, pascal, snake, kebab, constant.\nPreset selects a saved preset while idle. Style applies to the next recording.\nspeakeasy --version prints the version.\nspeakeasy show --help lists the Show commands."
    )
}

fn print_version() -> io::Result<()> {
    writeln!(
        io::stdout().lock(),
        "speakeasy {}",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(any(target_os = "windows", test))]
fn sign_in_launch(
    mode: LaunchMode,
    default_settings: bool,
    startup: &Startup,
    config: &Config,
) -> Option<bool> {
    (mode == LaunchMode::Live && default_settings && !matches!(startup, Startup::Invalid(_)))
        .then_some(config.start_at_login)
}

fn split_startup(startup: Startup) -> (Option<ValidConfig>, Snapshot) {
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
        Startup::Configured(valid) => (Some(*valid), placeholder()),
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

fn release_services(
    cx: &mut App,
    #[cfg(target_os = "windows")] handoff: std::sync::mpsc::Sender<updates::Installer>,
) -> impl Future<Output = ()> + use<> {
    if cx.has_global::<tray::Tray>() {
        drop(cx.remove_global::<tray::Tray>());
    }
    let services = cx.remove_global::<Services>();
    #[cfg(target_os = "windows")]
    let mut services = services;
    #[cfg(target_os = "windows")]
    let installer = services.take_installer();
    let instance = services.into_instance();

    async move {
        if let Some(instance) = instance {
            instance.close().await;
        }
        #[cfg(target_os = "windows")]
        if let Some(installer) = installer {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A departed composition root cannot install; dropping a refused handoff cleans its staging"
            )]
            let _ = handoff.send(installer);
        }
    }
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
    fn only_a_live_launch_of_readable_default_settings_touches_the_sign_in_launch() {
        use LaunchMode::{Demo, DemoTray, Live};
        let (default, other) = (true, false);
        let on = Config {
            start_at_login: true,
            ..Config::default()
        };
        let configured = Startup::Configured(Box::new(ValidConfig::assume(on.clone())));
        assert_eq!(sign_in_launch(Live, default, &configured, &on), Some(true));
        assert_eq!(sign_in_launch(Live, other, &configured, &on), None);
        let off = Config::default();
        let unconfigured = Startup::Unconfigured;
        assert_eq!(
            sign_in_launch(Live, default, &unconfigured, &off),
            Some(false)
        );
        assert_eq!(sign_in_launch(Live, other, &unconfigured, &off), None);
        let invalid = Startup::Invalid("Invalid settings".into());
        assert_eq!(sign_in_launch(Live, default, &invalid, &off), None);
        for demo in [Demo, DemoTray] {
            assert_eq!(sign_in_launch(demo, default, &unconfigured, &off), None);
        }
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
