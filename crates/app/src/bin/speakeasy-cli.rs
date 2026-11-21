#![forbid(unsafe_code)]

use std::{
    env,
    ffi::OsString,
    fmt::Display,
    io::{self, Write as _},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(first) = args.next() else {
        return help();
    };
    match first.to_str() {
        Some("--help" | "-h") => help(),
        Some("show" | "--version") => relay(first, args, Route::Utility),
        Some("status" | "dictation" | "paste-last" | "style" | "preset") => {
            relay(first, args, Route::Control)
        },
        _ => fail(
            "Use show, status, dictation, paste-last, style, preset, --version, or --help. Start \
             the desktop app with speakeasy.",
            2,
        ),
    }
}

enum Route {
    Utility,
    Control,
}

fn relay(first: OsString, rest: impl Iterator<Item = OsString>, route: Route) -> ExitCode {
    let unavailable = match route {
        Route::Utility => 1,
        Route::Control => 2,
    };
    let sibling = match env::current_exe() {
        Ok(exe) => exe.with_file_name(format!("speakeasy{}", env::consts::EXE_SUFFIX)),
        Err(error) => return fail(format!("Cannot locate Speakeasy: {error}"), unavailable),
    };
    match Command::new(&sibling).arg(first).args(rest).status() {
        Ok(status) => match status.code().and_then(|code| u8::try_from(code).ok()) {
            Some(code) if matches!(route, Route::Utility) || code <= 2 => ExitCode::from(code),
            _ => fail(
                format!("Speakeasy stopped unexpectedly ({status})."),
                unavailable,
            ),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => fail(
            format!(
                "speakeasy-cli needs {} beside it; extract the whole package again.",
                sibling.display()
            ),
            unavailable,
        ),
        Err(error) => fail(format!("Cannot start Speakeasy: {error}"), unavailable),
    }
}

fn help() -> ExitCode {
    match writeln!(
        io::stdout().lock(),
        "speakeasy-cli show [COMMAND]\n\
         speakeasy-cli status [--config PATH]\n\
         speakeasy-cli dictation start [--print] [--style STYLE] [--config PATH]\n\
         speakeasy-cli dictation stop TOKEN [--print] [--config PATH]\n\
         speakeasy-cli dictation cancel TOKEN [--config PATH]\n\
         speakeasy-cli paste-last [--config PATH]\n\
         speakeasy-cli style STYLE [--config PATH]\n\
         speakeasy-cli preset NAME [--config PATH]\n\
         speakeasy-cli --version\n\n\
         Control commands return JSON: exit 0 acknowledged, 1 refused, 2 unknown or not \
         running.\n\
         show --help lists the Show commands. Start the desktop app with speakeasy."
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(format!("Cannot write help: {error}"), 1),
    }
}

fn fail(message: impl Display, status: u8) -> ExitCode {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "stderr is the last channel left to report a failed command"
    )]
    let _ = writeln!(io::stderr().lock(), "speakeasy-cli: {message}");
    ExitCode::from(status)
}
