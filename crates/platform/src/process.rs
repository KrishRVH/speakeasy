//! Termination authority over the local speech worker and every process it starts.
//!
//! Authority comes only from a freshly spawned child's identifier, never from a process lookup.

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
