#[cfg(target_os = "windows")]
pub struct ProcessGroup(std::os::windows::io::OwnedHandle);

#[cfg(target_os = "windows")]
impl ProcessGroup {
    pub fn attach(pid: u32) -> anyhow::Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows_sys::Win32::System::{JobObjects::*, Threading::*};
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
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
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
    pub fn terminate(&mut self) {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the retained job contains only the child assigned above.
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0.as_raw_handle(), 1);
        }
    }
}

#[cfg(unix)]
pub struct ProcessGroup(Option<i32>);

#[cfg(unix)]
impl ProcessGroup {
    /// The caller spawns the child with process_group(0) and terminates the
    /// group before reaping its leader. No process-name lookup is used.
    pub fn attach(pid: u32) -> anyhow::Result<Self> {
        Ok(Self(Some(i32::try_from(pid)?)))
    }
    pub fn terminate(&mut self) {
        if let Some(pid) = self.0.take() {
            // SAFETY: this positive child PID identifies the explicitly created
            // process group. Clear it before signaling to avoid a second kill.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
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
