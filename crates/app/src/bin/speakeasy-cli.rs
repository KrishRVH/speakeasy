//! Console entry for Show tools. Inherited handles and an awaited child make the GUI executable's
//! output and exit status usable from Windows shells without changing desktop launch behavior.

#![forbid(unsafe_code)]

use std::{
    env,
    io::{self, Write as _},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let first = args.next();
    match first.as_deref().and_then(std::ffi::OsStr::to_str) {
        None if first.is_none() => return help(),
        Some("--help" | "-h") => return help(),
        Some("show" | "--version") => {},
        _ => {
            return fail(
                "Use show, --version, or --help. Start the desktop app with speakeasy.",
                2,
            );
        },
    }
    let sibling = match env::current_exe() {
        Ok(exe) => exe.with_file_name(format!("speakeasy{}", env::consts::EXE_SUFFIX)),
        Err(error) => return fail(format!("Cannot locate Speakeasy: {error}"), 1),
    };
    // status inherits the caller's streams, environment and working directory. No shell rewrites
    // paths or phrase arguments, and the console parent waits for the GUI child to finish.
    match Command::new(&sibling)
        .args(first.into_iter().chain(args))
        .status()
    {
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
        "speakeasy-cli show [COMMAND]\nspeakeasy-cli --version\n\nshow --help lists the Show tools. Start the desktop app with speakeasy."
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
