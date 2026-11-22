use std::{ffi::OsStr, process::Stdio};

use tokio::process::{Child, Command};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

pub(crate) async fn kill_and_reap(child: &mut Child) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Killing a child that already exited fails harmlessly; the wait still reaps it"
    )]
    let _ = child.start_kill();
    #[expect(
        clippy::let_underscore_must_use,
        reason = "The kill is sent and no caller can act on a failed wait; Tokio reaps the dropped child on a best-effort basis"
    )]
    let _ = child.wait().await;
}
