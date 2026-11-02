#![forbid(unsafe_code)]

use std::{
    env,
    io::{self, Write as _},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(first) = args.next() else {
        return help();
    };
    match first.to_str() {
        Some("--help" | "-h") => return help(),
        Some("show" | "--version" | "status" | "dictation" | "paste-last" | "style" | "preset") => {
        },
        _ => {
            return fail(
                "Use show, status, dictation, paste-last, style, preset, --version, or --help. Start the desktop app with speakeasy.",
                2,
            );
        },
    }
    let sibling = match env::current_exe() {
        Ok(exe) => exe.with_file_name(format!("speakeasy{}", env::consts::EXE_SUFFIX)),
        Err(error) => return fail(format!("Cannot locate Speakeasy: {error}"), 1),
    };

    match Command::new(&sibling).arg(first).args(args).status() {
        Ok(status) => match status.code().and_then(|code| u8::try_from(code).ok()) {
            Some(code) => ExitCode::from(code),
            None => fail(format!("Speakeasy stopped unexpectedly ({status})."), 1),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => fail(
            format!(
                "speakeasy-cli needs {} beside it; extract the whole package again.",
                sibling.display()
            ),
            1,
        ),
        Err(error) => fail(format!("Cannot start Speakeasy: {error}"), 1),
    }
}

fn help() -> ExitCode {
    match writeln!(
        io::stdout().lock(),
        "speakeasy-cli show [COMMAND]\nspeakeasy-cli status [--config PATH]\nspeakeasy-cli dictation start [--style STYLE] [--config PATH]\nspeakeasy-cli dictation stop|cancel TOKEN [--config PATH]\nspeakeasy-cli paste-last [--config PATH]\nspeakeasy-cli style STYLE [--config PATH]\nspeakeasy-cli preset NAME [--config PATH]\nspeakeasy-cli --version\n\nControl commands return JSON: exit 0 acknowledged, 1 refused, 2 unknown or not running.\nshow --help lists the Show tools. Start the desktop app with speakeasy."
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(format!("Cannot write help: {error}"), 1),
    }
}

fn fail(message: impl std::fmt::Display, status: u8) -> ExitCode {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "stderr is the last channel left to report a failed command"
    )]
    let _ = writeln!(io::stderr().lock(), "speakeasy-cli: {message}");
    ExitCode::from(status)
}
