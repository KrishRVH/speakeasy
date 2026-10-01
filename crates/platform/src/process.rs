//! Termination authority over the local speech worker and every process it starts.
//!
//! Authority comes only from a freshly spawned child's identifier, never from a process lookup.

#[cfg(target_os = "windows")]
use std::{
    os::windows::io::{AsRawHandle, OwnedHandle},
    ptr::null,
};

#[cfg(target_os = "windows")]
use windows_sys::Win32::System::{
    JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    },
    Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
};

#[cfg(target_os = "windows")]
use crate::windows::{ok_or_last_error, take_handle};

/// Owns a kill-on-close job containing the local speech worker.
#[cfg(target_os = "windows")]
pub struct ProcessGroup(OwnedHandle);

#[cfg(target_os = "windows")]
impl ProcessGroup {
    /// Attaches to a freshly spawned child, never an unrelated process.
    ///
    /// # Errors
    /// Returns an error if the process identifier or native group setup is invalid.
    pub fn attach(pid: u32) -> anyhow::Result<Self> {
        // SAFETY: null attributes and name create an unnamed job that only this group owns.
        let job = unsafe { take_handle(CreateJobObjectW(null(), null())) }?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = u32::try_from(size_of_val(&limits))?;
        // SAFETY: the job handle is open and `limits` outlives this call, which reads `size` bytes.
        ok_or_last_error(unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size,
            )
        })?;
        // SAFETY: a successful OpenProcess returns a fresh handle that only this function owns.
        let process =
            unsafe { take_handle(OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid)) }?;
        // SAFETY: both handles are open for this call; the job then holds only this child.
        ok_or_last_error(unsafe {
            AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle())
        })?;
        Ok(Self(job))
    }

    /// Terminates only the owned child group; repeated calls have no effect.
    pub fn terminate(&mut self) {
        // SAFETY: the retained job contains only the child assigned in `attach`.
        unsafe {
            // A failure needs no handling: closing the job on drop still kills the group.
            TerminateJobObject(self.0.as_raw_handle(), 1);
        }
    }
}

/// Owns termination of a child process group until its leader is reaped.
#[cfg(unix)]
pub struct ProcessGroup(Option<libc::pid_t>);

#[cfg(unix)]
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

#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(all(test, unix))]
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
