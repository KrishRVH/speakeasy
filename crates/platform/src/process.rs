#[cfg(target_os = "linux")]
use std::fs;
#[cfg(unix)]
use std::mem::MaybeUninit;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::net::Ipv4Addr;
#[cfg(target_os = "windows")]
use std::{
    io,
    os::windows::io::{AsRawHandle, BorrowedHandle, OwnedHandle},
    ptr::null,
};

use anyhow::Context as _;
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
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    },
};

#[cfg(target_os = "windows")]
use crate::windows::{ok_or_last_error, take_handle};

#[cfg(any(target_os = "linux", target_os = "windows"))]
const LOOPBACK: u32 = u32::from_ne_bytes(Ipv4Addr::LOCALHOST.octets());

#[cfg(target_os = "windows")]
#[derive(Debug)]
pub struct ProcessGroup {
    job: OwnedHandle,
}

#[cfg(target_os = "windows")]
impl ProcessGroup {
    pub fn attach(child: &tokio::process::Child) -> anyhow::Result<Self> {
        let process = child
            .raw_handle()
            .context("The child exited before it could be contained")?;

        Self::contain(unsafe { BorrowedHandle::borrow_raw(process) })
    }

    fn contain(process: BorrowedHandle<'_>) -> anyhow::Result<Self> {
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

        ok_or_last_error(unsafe {
            AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle())
        })?;
        Ok(Self { job })
    }

    pub fn terminate(&mut self) {
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 1);
        }
    }

    pub fn holds_loopback_listener(&self, port: u16) -> anyhow::Result<bool> {
        let owners = loopback_listener_owners(port)?;
        Ok(!owners.is_empty() && owners.into_iter().all(|owner| self.contains(owner)))
    }

    fn contains(&self, pid: u32) -> bool {
        let Ok(process) =
            (unsafe { take_handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid)) })
        else {
            return false;
        };
        let mut member = FALSE;

        let queried = unsafe {
            IsProcessInJob(
                process.as_raw_handle(),
                self.job.as_raw_handle(),
                &raw mut member,
            )
        };
        queried != FALSE && member != FALSE
    }
}

#[cfg(target_os = "windows")]
const ROW_WORDS: usize = 6;
#[cfg(target_os = "windows")]
const _: () = assert!(size_of::<MIB_TCPROW_OWNER_PID>() == ROW_WORDS * size_of::<u32>());

#[cfg(target_os = "windows")]
fn loopback_listener_owners(port: u16) -> anyhow::Result<Vec<u32>> {
    const AF_INET: u32 = 2;

    const ATTEMPTS: usize = 4;
    let mut table: Vec<u32> = Vec::new();
    let mut size = 0_u32;
    for _ in 0..ATTEMPTS {
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
            ERROR_INSUFFICIENT_BUFFER => {
                table.resize(usize::try_from(size)?.div_ceil(size_of::<u32>()), 0);
            },
            error => return Err(io::Error::from_raw_os_error(error.cast_signed()).into()),
        }
    }
    anyhow::bail!("The system's listener table kept changing")
}

#[cfg(target_os = "windows")]
fn listener_owners(table: &[u32], port: u16) -> Vec<u32> {
    let Some((&count, rows)) = table.split_first() else {
        return Vec::new();
    };
    rows.as_chunks::<ROW_WORDS>()
        .0
        .iter()
        .take(usize::try_from(count).unwrap_or(usize::MAX))
        .filter(|&&[_, address, local_port, ..]| {
            let [high, low, ..] = local_port.to_ne_bytes();
            address == LOOPBACK && u16::from_be_bytes([high, low]) == port
        })
        .map(|&[.., owner]| owner)
        .collect()
}

#[cfg(unix)]
#[derive(Debug)]
pub struct ProcessGroup {
    leader: Option<libc::pid_t>,
}

#[cfg(unix)]
impl ProcessGroup {
    pub fn attach(child: &tokio::process::Child) -> anyhow::Result<Self> {
        Self::led_by(
            child
                .id()
                .context("The child exited before it could be contained")?,
        )
    }

    fn led_by(pid: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(
            pid > 1,
            "Child process identifier must identify a spawned child"
        );
        Ok(Self {
            leader: Some(libc::pid_t::try_from(pid)?),
        })
    }

    pub fn terminate(&mut self) {
        if let Some(leader) = self.leader.take().filter(|&leader| unreaped(leader)) {
            unsafe {
                libc::killpg(leader, libc::SIGKILL);
            }
        }
    }
}

#[cfg(unix)]
fn unreaped(pid: libc::pid_t) -> bool {
    let mut info = MaybeUninit::<libc::siginfo_t>::uninit();

    unsafe {
        libc::waitid(
            libc::P_PID,
            pid.cast_unsigned(),
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        ) == 0
    }
}

#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(target_os = "linux")]
impl ProcessGroup {
    pub fn holds_loopback_listener(&self, port: u16) -> anyhow::Result<bool> {
        let Some(leader) = self.leader.filter(|&leader| unreaped(leader)) else {
            return Ok(false);
        };
        let mut unheld = loopback_listeners(&fs::read_to_string("/proc/net/tcp")?, port);
        if unheld.is_empty() {
            return Ok(false);
        }

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

#[cfg(target_os = "linux")]
fn loopback_listeners(table: &str, port: u16) -> Vec<u64> {
    const LISTEN: &str = "0A";
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
                && u32::from_str_radix(address, 16).ok()? == LOOPBACK
                && u16::from_str_radix(local_port, 16).ok()? == port;
            listening.then_some(inode)?.parse().ok()
        })
        .collect()
}

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

#[cfg(target_os = "linux")]
fn process_group(pid: libc::pid_t) -> Option<libc::pid_t> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    fields.split_whitespace().nth(2)?.parse().ok()
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
            assert!(
                ProcessGroup::led_by(pid).is_err(),
                "Accepted invalid child identifier {pid}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn terminating_kills_the_group_its_unreaped_child_leads() -> anyhow::Result<()> {
        use std::{
            os::unix::process::{CommandExt as _, ExitStatusExt as _},
            process::Command,
        };

        let mut child = Command::new("sleep").arg("60").process_group(0).spawn()?;
        ProcessGroup::led_by(child.id())?.terminate();
        assert_eq!(child.wait()?.signal(), Some(libc::SIGKILL));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_group_led_by_a_process_this_one_never_spawned_is_never_signaled() -> anyhow::Result<()> {
        use std::{
            io::{BufRead as _, BufReader},
            os::fd::OwnedFd,
            process::{Command, Stdio},
            thread,
            time::{Duration, Instant},
        };

        use anyhow::Context as _;

        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = held.local_addr()?.port();

        let mut parent = Command::new("sh")
            .args([
                "-c",
                "exec 3<&0 0</dev/null; setsid sleep 60 <&3 3<&- & exec 3<&-; echo $!; wait $!; \
                 echo $?",
            ])
            .stdin(Stdio::from(OwnedFd::from(held)))
            .stdout(Stdio::piped())
            .spawn()?;
        let mut reports =
            BufReader::new(parent.stdout.take().context("Parent output missing")?).lines();
        let stranger = reports.next().context("No identifier")??.parse()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while super::process_group(stranger) != Some(stranger) {
            anyhow::ensure!(Instant::now() < deadline, "It never led its own group");
            thread::sleep(Duration::from_millis(5));
        }
        let mut group = ProcessGroup::led_by(stranger.cast_unsigned())?;
        let counted = group.holds_loopback_listener(port);
        group.terminate();
        Command::new("kill").arg(stranger.to_string()).status()?;
        let ended = reports.next().context("No status")??;
        parent.wait()?;
        assert!(!counted?, "Its listener counted");
        assert_eq!(ended, "143", "Ended by SIGTERM, not SIGKILL");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_listeners_held_inside_the_group_count() -> anyhow::Result<()> {
        use std::{
            os::{fd::OwnedFd, unix::process::CommandExt as _},
            process::{Command, Stdio},
        };

        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = held.local_addr()?.port();

        let client = TcpStream::connect(held.local_addr()?)?;
        let foreign = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;

        let mut child = Command::new("sleep")
            .arg("60")
            .process_group(0)
            .stdin(Stdio::from(OwnedFd::from(held)))
            .spawn()?;
        let mut group = ProcessGroup::led_by(child.id())?;
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

    #[cfg(target_os = "linux")]
    #[test]
    fn listeners_held_by_members_other_than_the_leader_count() -> anyhow::Result<()> {
        use std::{
            io::{BufRead as _, BufReader},
            os::{fd::OwnedFd, unix::process::CommandExt as _},
            process::{Command, Stdio},
        };

        use anyhow::Context as _;

        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = held.local_addr()?.port();

        let mut leader = Command::new("sh")
            .args([
                "-c",
                "exec 3<&0; (sleep 60 <&3 3<&- &); exec 0</dev/null 3<&-; echo; exec sleep 60",
            ])
            .process_group(0)
            .stdin(Stdio::from(OwnedFd::from(held)))
            .stdout(Stdio::piped())
            .spawn()?;
        let mut group = ProcessGroup::led_by(leader.id())?;
        let handed_over = leader.stdout.take().context("Leader output missing")?;
        BufReader::new(handed_over).read_line(&mut String::new())?;
        assert!(group.holds_loopback_listener(port)?);
        group.terminate();
        leader.wait()?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn only_listeners_held_inside_the_job_count() -> anyhow::Result<()> {
        use std::{
            io::{BufRead as _, BufReader},
            os::windows::io::AsHandle as _,
            process::{Command, Stdio},
        };

        use anyhow::Context as _;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let client = TcpStream::connect(listener.local_addr()?)?;
        assert_eq!(super::loopback_listener_owners(port)?, [std::process::id()]);
        assert_eq!(
            super::loopback_listener_owners(client.local_addr()?.port())?,
            Vec::<u32>::new()
        );

        let mut child = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0); \
                 $listener.Start(); [Console]::Out.WriteLine($listener.LocalEndpoint.Port); \
                 [Console]::Out.Flush(); Start-Sleep 60",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
        let group = ProcessGroup::contain(child.as_handle())?;
        let mut reported = String::new();
        BufReader::new(child.stdout.take().context("Child output missing")?)
            .read_line(&mut reported)?;
        let held = reported.trim().parse()?;
        assert!(group.holds_loopback_listener(held)?);
        assert!(!group.holds_loopback_listener(port)?);

        drop(group);
        child.wait()?;
        Ok(())
    }
}
