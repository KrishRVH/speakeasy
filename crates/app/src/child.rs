//! Child processes Speakeasy owns: hidden from the desktop, killed with their owner, and reaped
//! before anything replaces them.

use std::{ffi::OsStr, process::Stdio};

use tokio::process::{Child, Command};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command without a console window or standard streams, killed if its `Child` drops. Engines
/// may print dictated text, so diagnostics report only exit statuses; a caller that parses output
/// pipes stdout.
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

/// Kills `child` and waits, without a time limit, until it is reaped.
#[expect(
    clippy::let_underscore_must_use,
    reason = "Killing an exited child fails harmlessly, and waiting reaps it either way"
)]
pub(crate) async fn kill_and_reap(child: &mut Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::path::Path;

    use anyhow::Context as _;

    use super::*;

    #[tokio::test]
    async fn owned_children_cannot_read_or_print_to_speakeasys_streams() -> anyhow::Result<()> {
        let mut command = hidden_command("sleep");
        command.arg("60");
        let mut child = command.spawn()?;
        let pid = child.id().context("The fixture child exited at once")?;
        let streams = (0..3)
            .map(|fd| std::fs::read_link(format!("/proc/{pid}/fd/{fd}")))
            .collect::<std::io::Result<Vec<_>>>();
        kill_and_reap(&mut child).await;
        for stream in streams? {
            assert_eq!(stream, Path::new("/dev/null"));
        }
        Ok(())
    }
}
