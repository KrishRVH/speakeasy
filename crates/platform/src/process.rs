//! Termination authority over the local speech worker and every process it starts, and the check
//! that the worker's loopback listener belongs to it.
//!
//! Authority comes only from a freshly spawned child's identifier, never from a process lookup.
//! Listener lookups only test membership in the group and never grant termination authority.

#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "windows")]
use std::{
    io,
    os::windows::io::{AsRawHandle, OwnedHandle},
    ptr::null,
};

#[cfg(target_os = "windows")]
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, FALSE, NO_ERROR},
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    },
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        },
    },
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

    /// Records that the caller reaped the group leader. A job names its members by handle, not by
    /// process identifier, so it keeps terminating any surviving children; nothing changes.
    pub fn leader_reaped(&mut self) {}

    /// Reports whether at least one IPv4 listener exists on 127.0.0.1:`port` and every one belongs
    /// to a process in this job.
    ///
    /// # Errors
    /// Returns an error if the system's listener table cannot be read.
    pub fn holds_loopback_listener(&self, port: u16) -> anyhow::Result<bool> {
        let owners = loopback_listener_owners(port)?;
        Ok(!owners.is_empty() && owners.into_iter().all(|owner| self.contains(owner)))
    }

    /// Whether `pid` names a process in this job; a process this account cannot open is foreign.
    fn contains(&self, pid: u32) -> bool {
        // SAFETY: a successful OpenProcess returns a fresh handle that only this function owns.
        let Ok(process) =
            (unsafe { take_handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)) })
        else {
            return false;
        };
        let mut member = FALSE;
        // SAFETY: both handles are open for this call, and `member` outlives it.
        let queried = unsafe {
            IsProcessInJob(
                process.as_raw_handle(),
                self.0.as_raw_handle(),
                &raw mut member,
            )
        };
        queried != FALSE && member != FALSE
    }
}

/// `MIB_TCPTABLE_OWNER_PID` is an entry count followed by rows of six `DWORD`s: state, local
/// address, local port, remote address, remote port, and owning process. Addresses are in network
/// byte order, and a port fills the two low-addressed bytes of its `DWORD`.
#[cfg(target_os = "windows")]
const ROW_WORDS: usize = 6;
#[cfg(target_os = "windows")]
const _: () = assert!(size_of::<MIB_TCPROW_OWNER_PID>() == ROW_WORDS * size_of::<u32>());

/// The processes listening on 127.0.0.1:`port`.
#[cfg(target_os = "windows")]
fn loopback_listener_owners(port: u16) -> anyhow::Result<Vec<u32>> {
    /// Winsock's IPv4 address family, the only one the engine binds.
    const AF_INET: u32 = 2;
    /// A table that keeps outgrowing each new buffer is not worth chasing further.
    const ATTEMPTS: usize = 4;
    let mut table: Vec<u32> = Vec::new();
    let mut size = 0_u32;
    for _ in 0..ATTEMPTS {
        // SAFETY: `table` is writable and suitably aligned for at least `size` bytes, and `size`
        // outlives this call, which reads it and on a short buffer stores the size required.
        let status = unsafe {
            GetExtendedTcpTable(
                table.as_mut_ptr().cast(),
                &raw mut size,
                FALSE,
                AF_INET,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        match status {
            NO_ERROR => return Ok(listener_owners(&table, port)),
            ERROR_INSUFFICIENT_BUFFER => table.resize(usize::try_from(size.div_ceil(4))?, 0),
            error => return Err(io::Error::from_raw_os_error(error.cast_signed()).into()),
        }
    }
    anyhow::bail!("The system's listener table kept changing")
}

/// The owning processes of the rows in `table` that listen on 127.0.0.1:`port`.
#[cfg(target_os = "windows")]
fn listener_owners(table: &[u32], port: u16) -> Vec<u32> {
    let Some((&count, rows)) = table.split_first() else {
        return Vec::new();
    };
    let loopback = u32::from_ne_bytes([127, 0, 0, 1]);
    rows.as_chunks::<ROW_WORDS>()
        .0
        .iter()
        .take(usize::try_from(count).unwrap_or(usize::MAX))
        .filter(|&&[_, address, local_port, ..]| {
            let [high, low, ..] = local_port.to_ne_bytes();
            address == loopback && u16::from_be_bytes([high, low]) == port
        })
        .map(|&[.., owner]| owner)
        .collect()
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

    /// Records that the caller reaped the group leader. The leader's identifier may now name an
    /// unrelated process group, so this group is never signaled again.
    pub fn leader_reaped(&mut self) {
        self.0 = None;
    }
}

#[cfg(target_os = "linux")]
impl ProcessGroup {
    /// Reports whether at least one IPv4 listener exists on 127.0.0.1:`port` and every one is held
    /// by a process in this group. A group that was terminated or whose leader was reaped holds none.
    ///
    /// # Errors
    /// Returns an error if the system's socket table cannot be read.
    pub fn holds_loopback_listener(&self, port: u16) -> anyhow::Result<bool> {
        let Some(leader) = self.0 else {
            return Ok(false);
        };
        let mut unheld = loopback_listeners(&fs::read_to_string("/proc/net/tcp")?, port);
        if unheld.is_empty() {
            return Ok(false);
        }
        // Engines serve from the leader; the whole group is searched only for a wrapper that does
        // not exec its engine.
        release_held(leader, &mut unheld);
        if !unheld.is_empty() {
            for member in fs::read_dir("/proc")?
                .flatten()
                .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
                .filter(|&pid| process_group(pid) == Some(leader))
            {
                release_held(member, &mut unheld);
            }
        }
        Ok(unheld.is_empty())
    }
}

/// The inodes of IPv4 sockets listening on 127.0.0.1:`port` in a `/proc/net/tcp` table, which
/// prints each local address and port as hexadecimal in host byte order.
#[cfg(target_os = "linux")]
fn loopback_listeners(table: &str, port: u16) -> Vec<u64> {
    const LISTEN: &str = "0A";
    let loopback = u32::from_ne_bytes([127, 0, 0, 1]);
    table
        .lines()
        .skip(1)
        .filter_map(|row| {
            let fields: Vec<&str> = row.split_whitespace().collect();
            let [_, local, _, state, _, _, _, _, _, inode, ..] = fields.as_slice() else {
                return None;
            };
            let (address, local_port) = local.split_once(':')?;
            let listening = *state == LISTEN
                && u32::from_str_radix(address, 16).ok()? == loopback
                && u16::from_str_radix(local_port, 16).ok()? == port;
            listening.then(|| inode.parse().ok()).flatten()
        })
        .collect()
}

/// Removes from `unheld` each socket inode that process `pid` holds. A process that has exited or
/// that this account cannot inspect holds none.
#[cfg(target_os = "linux")]
fn release_held(pid: libc::pid_t, unheld: &mut Vec<u64>) {
    let Ok(descriptors) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return;
    };
    for descriptor in descriptors.flatten() {
        let inode = fs::read_link(descriptor.path()).ok().and_then(|target| {
            target
                .to_str()?
                .strip_prefix("socket:[")?
                .strip_suffix(']')?
                .parse::<u64>()
                .ok()
        });
        if let Some(inode) = inode {
            unheld.retain(|&listener| listener != inode);
        }
    }
}

/// The process group of `pid`, read after the parenthesized command name, which may contain
/// spaces and parentheses.
#[cfg(target_os = "linux")]
fn process_group(pid: libc::pid_t) -> Option<libc::pid_t> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    fields.split_whitespace().nth(2)?.parse().ok()
}

#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    use super::ProcessGroup;

    #[cfg(unix)]
    #[test]
    fn invalid_process_ids_never_acquire_group_termination_authority() {
        for pid in [0, 1, u32::MAX] {
            let mut attached = ProcessGroup::attach(pid);
            // Release authority before dropping so even a regression cannot signal the test
            // runner's group.
            if let Ok(group) = &mut attached {
                group.leader_reaped();
            }
            assert!(attached.is_err(), "Accepted invalid child identifier {pid}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_listeners_held_inside_the_group_count() -> anyhow::Result<()> {
        use std::{
            os::{fd::OwnedFd, unix::process::CommandExt},
            process::{Command, Stdio},
        };

        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = held.local_addr()?.port();
        // The queued connection adds an established socket on the held port without an owner.
        let client = TcpStream::connect(held.local_addr()?)?;
        let foreign = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        // The child's standard input becomes the only descriptor for the held listener.
        let mut child = Command::new("sleep")
            .arg("60")
            .process_group(0)
            .stdin(Stdio::from(OwnedFd::from(held)))
            .spawn()?;
        let mut group = ProcessGroup::attach(child.id())?;
        assert!(group.holds_loopback_listener(port)?);
        for other in [foreign.local_addr()?.port(), client.local_addr()?.port()] {
            assert!(
                !group.holds_loopback_listener(other)?,
                "Port {other} counted"
            );
        }
        group.terminate();
        child.wait()?;
        assert!(!group.holds_loopback_listener(port)?);
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn only_listeners_held_inside_the_job_count() -> anyhow::Result<()> {
        use std::process::{Command, Stdio};

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let client = TcpStream::connect(listener.local_addr()?)?;
        assert_eq!(super::loopback_listener_owners(port)?, [std::process::id()]);
        assert_eq!(
            super::loopback_listener_owners(client.local_addr()?.port())?,
            Vec::<u32>::new()
        );
        let mut child = Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()?;
        let group = ProcessGroup::attach(child.id())?;
        assert!(!group.holds_loopback_listener(port)?);
        // Closing the kill-on-close job ends the child.
        drop(group);
        child.wait()?;
        Ok(())
    }
}
