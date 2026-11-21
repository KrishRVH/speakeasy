#![forbid(unsafe_code)]

use std::{
    env,
    ffi::OsString,
    fmt::Display,
    io::{self, Write as _},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1).peekable();
    match args.peek().map(|first| first.to_str()) {
        None | Some(Some("--help" | "-h")) => help(),
        Some(Some(
            "show" | "--version" | "status" | "dictation" | "paste-last" | "style" | "preset",
        )) => relay(args),
        Some(_) => fail(
            "Use show, status, dictation, paste-last, style, preset, --version, or --help. Start \
             the desktop app with speakeasy.",
        ),
    }
}

fn relay(args: impl Iterator<Item = OsString>) -> ExitCode {
    let sibling = match env::current_exe() {
        Ok(exe) => exe.with_file_name(format!("speakeasy{}", env::consts::EXE_SUFFIX)),
        Err(error) => return fail(format!("Cannot locate Speakeasy: {error}")),
    };
    match Command::new(&sibling).args(args).status() {
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or_else(
                || fail(format!("Speakeasy stopped unexpectedly ({status}).")),
                ExitCode::from,
            ),
        Err(error) => fail(format!(
            "Cannot start {}: {error}. Keep speakeasy-cli in the same folder as Speakeasy.",
            sibling.display()
        )),
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
        Err(error) => fail(format!("Cannot write help: {error}")),
    }
}

fn fail(message: impl Display) -> ExitCode {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "stderr is the last channel left to report a failed command"
    )]
    let _ = writeln!(io::stderr().lock(), "speakeasy-cli: {message}");
    ExitCode::from(2)
}
