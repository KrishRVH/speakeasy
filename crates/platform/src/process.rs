#[cfg(unix)]
use std::{io, mem::MaybeUninit};
#[cfg(target_os = "windows")]
use std::{
    os::windows::io::{AsRawHandle, OwnedHandle},
    ptr::null,
};

use anyhow::Context as _;
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};

#[cfg(target_os = "windows")]
use crate::windows::{ok_or_last_error, take_handle};

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub struct ProcessGroup {
    job: OwnedHandle,
}

#[cfg(target_os = "windows")]
impl ProcessGroup {
    /// Contains a freshly spawned child; attach right after spawning.
    ///
    /// Processes the child started before this call stay outside the job.
    pub fn attach(child: &tokio::process::Child) -> anyhow::Result<Self> {
        let process = child
            .raw_handle()
            .context("The child exited before it could be contained")?;
        let job = unsafe { take_handle(CreateJobObjectW(null(), null())) }?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = u32::try_from(size_of_val(&limits))?;

        ok_or_last_error(unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size,
            )
        })?;

        // SAFETY: `child` keeps `process` open through this call.
        ok_or_last_error(unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) })?;
        Ok(Self { job })
    }

    pub fn terminate(&mut self) {
        unsafe {
            // A failure needs no handling: closing the job on drop still kills the group.
            TerminateJobObject(self.job.as_raw_handle(), 1);
        }
    }
}

#[cfg(unix)]
#[derive(Debug)]
pub struct ProcessGroup {
    leader: Option<libc::pid_t>,
}

#[cfg(unix)]
impl ProcessGroup {
    /// Leads the group of a child spawned with `process_group(0)`, whose PID names the group.
    pub fn attach(child: &tokio::process::Child) -> anyhow::Result<Self> {
        let leader = child
            .id()
            .context("The child exited before it could be contained")?;
        Ok(Self {
            leader: Some(libc::pid_t::try_from(leader)?),
        })
    }

    pub fn leader_exited(&self) -> io::Result<bool> {
        let Some(leader) = self.leader else {
            return Ok(true);
        };
        loop {
            // Zeroed, not uninit: with WNOHANG and no exited child, waitid returns 0 and may leave
            // `info` untouched, so only a matching `si_pid` reports an exit. WNOWAIT leaves the
            // leader for its owner to reap, so its PID stays reserved until then.
            let mut info = MaybeUninit::<libc::siginfo_t>::zeroed();
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    leader.cast_unsigned(),
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                return Ok(unsafe { info.assume_init().si_pid() } == leader);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    /// Kills the group; call it before the leader is reaped.
    ///
    /// A reaped leader's PID can name another group. The leader is taken before signaling, so a
    /// second call or the drop never signals a reused PID.
    pub fn terminate(&mut self) {
        if let Some(leader) = self.leader.take() {
            unsafe {
                // A failure needs no handling: ESRCH means the group already exited.
                libc::killpg(leader, libc::SIGKILL);
            }
        }
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
    use std::os::unix::process::ExitStatusExt as _;

    use super::ProcessGroup;

    #[tokio::test]
    async fn terminating_kills_the_group_its_unreaped_child_leads() -> anyhow::Result<()> {
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()?;
        let mut group = ProcessGroup::attach(&child)?;
        assert!(!group.leader_exited()?);
        group.terminate();
        assert_eq!(child.wait().await?.signal(), Some(libc::SIGKILL));
        Ok(())
    }
}
