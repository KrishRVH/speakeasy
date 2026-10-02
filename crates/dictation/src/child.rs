//! Child processes Speakeasy owns: killed with their owner and reaped before anything replaces them.

use std::{ffi::OsStr, process::Stdio};

use tokio::process::{Child, Command};

/// A command with null stdin and stderr, killed if its `Child` drops.
pub(crate) fn owned_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
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
