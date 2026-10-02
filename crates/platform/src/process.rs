//! Termination authority over the local speech worker and every process it starts, and the
//! descriptor and exit primitives its helper process needs.
//!
//! Authority comes only from a freshly spawned child's identifier, never from a process lookup.

use std::{
    fs::File,
    io::{self, Stdout},
    os::fd::AsFd,
};

/// Owns termination of a child process group until its leader is reaped.
pub struct ProcessGroup(Option<libc::pid_t>);

impl ProcessGroup {
    /// Attaches to a freshly spawned child, never an unrelated process.
    ///
    /// The caller spawns the child with `process_group(0)`, so its identifier names its group, and
    /// terminates the group before reaping its leader.
    ///
    /// # Errors
    /// Returns an error if the process identifier or native group setup is invalid.
    pub fn attach(pid: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(
            pid > 1,
            "Child process identifier must identify a spawned child"
        );
        Ok(Self(Some(libc::pid_t::try_from(pid)?)))
    }

    /// Terminates only the owned child group; repeated calls have no effect.
    pub fn terminate(&mut self) {
        if let Some(group) = self.0.take() {
            // SAFETY: killpg takes plain values. `group` is above 1, so it names only the child's
            // own group, and ownership is cleared before signaling so no group is signaled twice.
            unsafe {
                // A failure needs no handling: ESRCH means the group has already exited.
                libc::killpg(group, libc::SIGKILL);
            }
        }
    }

    /// Releases termination authority once the group leader has been reaped.
    pub fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Moves standard output to a private descriptor and points descriptor 1 at standard error, so a
/// native library that prints cannot corrupt a protocol written to the returned file.
///
/// # Errors
/// Returns the operating system's refusal to duplicate either descriptor.
pub fn private_stdout(stdout: &Stdout) -> io::Result<File> {
    let private = stdout.as_fd().try_clone_to_owned()?;
    // SAFETY: dup2 takes plain descriptor numbers; both are open in every process, and it replaces
    // descriptor 1 atomically while `private` keeps the original pipe.
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(File::from(private))
}

/// Ends this process at once with `code`, skipping destructors and exit handlers.
///
/// A helper whose parent has gone may have another thread blocked inside native inference; running
/// the native library's exit-time destructors under that thread could crash the exit itself.
pub fn exit_now(code: i32) -> ! {
    // SAFETY: `_exit` takes a plain value and never returns; skipping Rust and C cleanup is its
    // purpose, and the OS reclaims every resource this process holds.
    unsafe { libc::_exit(code) }
}

#[cfg(test)]
mod tests {
    use super::ProcessGroup;

    #[test]
    fn invalid_process_ids_never_acquire_group_termination_authority() {
        for pid in [0, 1, u32::MAX] {
            let mut attached = ProcessGroup::attach(pid);
            // Disarm before dropping so even a regression cannot signal the test runner's group.
            if let Ok(group) = &mut attached {
                group.disarm();
            }
            assert!(attached.is_err(), "Accepted invalid child identifier {pid}");
        }
    }
}
