/// Owns a kill-on-close job containing the local speech worker.
#[cfg(target_os = "windows")]
pub struct ProcessGroup(std::os::windows::io::OwnedHandle);

#[cfg(target_os = "windows")]
impl ProcessGroup {
    /// Attach to a freshly spawned child, never an unrelated process.
    ///
    /// # Errors
    /// Returns an error if the process identifier or native group setup is invalid.
    pub fn attach(pid: u32) -> anyhow::Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows_sys::Win32::System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE},
        };
        // SAFETY: fresh native handles transfer immediately into OwnedHandle.
        // The job owns only this child; closing it kills its remaining members.
        unsafe {
            let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if raw.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            let job = std::os::windows::io::OwnedHandle::from_raw_handle(raw);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                u32::try_from(std::mem::size_of_val(&limits))?,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let raw_process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if raw_process.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            let process = std::os::windows::io::OwnedHandle::from_raw_handle(raw_process);
            if AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Self(job))
        }
    }
    /// Terminate only the owned child group; repeated calls have no effect.
    pub fn terminate(&mut self) {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the retained job contains only the child assigned above.
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0.as_raw_handle(), 1);
        }
    }
}

/// Owns termination of a child process group until its leader is reaped.
#[cfg(unix)]
pub struct ProcessGroup(Option<std::num::NonZeroI32>);

#[cfg(unix)]
impl ProcessGroup {
    /// The caller spawns the child with `process_group(0)` and terminates the
    /// group before reaping its leader. No process-name lookup is used.
    /// Attach to a freshly spawned child, never an unrelated process.
    ///
    /// # Errors
    /// Returns an error if the process identifier or native group setup is invalid.
    pub fn attach(pid: u32) -> anyhow::Result<Self> {
        use anyhow::{Context as _, ensure};
        ensure!(
            pid > 1,
            "Child process identifier must identify a spawned child"
        );
        let group = i32::try_from(pid)?
            .checked_neg()
            .context("Invalid child process identifier")?;
        let group = std::num::NonZeroI32::new(group)
            .context("Child process identifier must be positive")?;
        Ok(Self(Some(group)))
    }
    /// Terminate only the owned child group; repeated calls have no effect.
    pub fn terminate(&mut self) {
        if let Some(pid) = self.0.take() {
            // SAFETY: the negative nonzero ID identifies only the explicitly
            // created child group. Clear ownership before signaling a second time.
            unsafe {
                libc::kill(pid.get(), libc::SIGKILL);
            }
        }
    }
    /// Release termination authority after reaping the group leader.
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
            let mut result = ProcessGroup::attach(pid);
            // Even a regression must not signal the test runner’s process group.
            if let Ok(group) = &mut result {
                group.disarm();
            }
            assert!(result.is_err(), "Accepted invalid child identifier {pid}");
        }
    }
}
