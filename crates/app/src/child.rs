//! Child processes Speakeasy owns: hidden from the desktop, killed with their owner, and reaped
//! before anything replaces them.

use std::{ffi::OsStr, process::Stdio};

use tokio::process::{Child, Command};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command without a console window, with null stdin and stderr, killed if its `Child` drops.
pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Kills `child` and waits, without a time limit, until it is reaped.
#[expect(
    clippy::let_underscore_must_use,
    reason = "Killing an exited child fails harmlessly, and waiting reaps it either way"
)]
pub(crate) async fn kill_and_reap(child: &mut Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}
