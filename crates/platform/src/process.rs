#[cfg(target_os = "linux")]
use std::fs;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
#[cfg(unix)]
use std::{io, mem::MaybeUninit};
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
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopbackListener {
    Missing,
    Owned,
    Foreign,
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn classify<L: PartialEq>(
    mut listening: impl FnMut() -> anyhow::Result<Vec<L>>,
    mut unheld: impl FnMut(Vec<L>) -> anyhow::Result<Vec<L>>,
) -> anyhow::Result<LoopbackListener> {
    const ATTEMPTS: usize = 4;
    let mut listed = listening()?;
    for _ in 0..ATTEMPTS {
        if listed.is_empty() {
            return Ok(LoopbackListener::Missing);
        }
        let foreign = unheld(listed)?;
        if foreign.is_empty() {
            return Ok(LoopbackListener::Owned);
        }
        listed = listening()?;
        if foreign.iter().all(|listener| listed.contains(listener)) {
            return Ok(LoopbackListener::Foreign);
        }
    }
    anyhow::bail!("The listeners on the port kept changing")
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn reaches_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address == Ipv4Addr::LOCALHOST || address.is_unspecified(),
        IpAddr::V6(address) => {
            address.is_unspecified() || address.to_ipv4_mapped() == Some(Ipv4Addr::LOCALHOST)
        },
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn ipv6(words: [u32; 4]) -> Ipv6Addr {
    let mut octets = [0_u8; 16];
    for (quad, word) in octets.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        *quad = word.to_ne_bytes();
    }
    Ipv6Addr::from(octets)
}

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

    pub fn loopback_listener(&self, port: u16) -> anyhow::Result<LoopbackListener> {
        classify(
            || loopback_listener_owners(port),
            |mut owners| {
                owners.retain(|&owner| !self.contains(owner));
                Ok(owners)
            },
        )
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
const IPV4_ROW_WORDS: usize = 6;
#[cfg(target_os = "windows")]
const IPV6_ROW_WORDS: usize = 14;
#[cfg(target_os = "windows")]
const _: () = {
    use std::mem::offset_of;
    assert!(size_of::<MIB_TCPROW_OWNER_PID>() == size_of::<[u32; IPV4_ROW_WORDS]>());
    assert!(offset_of!(MIB_TCPROW_OWNER_PID, dwLocalAddr) == size_of::<[u32; 1]>());
    assert!(offset_of!(MIB_TCPROW_OWNER_PID, dwLocalPort) == size_of::<[u32; 2]>());
    assert!(offset_of!(MIB_TCPROW_OWNER_PID, dwOwningPid) == size_of::<[u32; 5]>());
    assert!(size_of::<MIB_TCP6ROW_OWNER_PID>() == size_of::<[u32; IPV6_ROW_WORDS]>());
    assert!(offset_of!(MIB_TCP6ROW_OWNER_PID, ucLocalAddr) == 0);
    assert!(offset_of!(MIB_TCP6ROW_OWNER_PID, dwLocalPort) == size_of::<[u32; 5]>());
    assert!(offset_of!(MIB_TCP6ROW_OWNER_PID, dwOwningPid) == size_of::<[u32; 13]>());
};

#[cfg(target_os = "windows")]
fn loopback_listener_owners(port: u16) -> anyhow::Result<Vec<u32>> {
    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;

    let mut owners = listener_owners(&listener_table(AF_INET)?, port, ipv4_row);
    owners.extend(listener_owners(&listener_table(AF_INET6)?, port, ipv6_row));
    Ok(owners)
}

#[cfg(target_os = "windows")]
struct Row {
    address: IpAddr,
    port: u16,
    owner: u32,
}

#[cfg(target_os = "windows")]
fn ipv4_row(&[_, address, port, _, _, owner]: &[u32; IPV4_ROW_WORDS]) -> Row {
    Row {
        address: IpAddr::from(address.to_ne_bytes()),
        port: network_port(port),
        owner,
    }
}

#[cfg(target_os = "windows")]
fn ipv6_row(&[a, b, c, d, _, port, .., owner]: &[u32; IPV6_ROW_WORDS]) -> Row {
    Row {
        address: IpAddr::V6(ipv6([a, b, c, d])),
        port: network_port(port),
        owner,
    }
}

#[cfg(target_os = "windows")]
fn network_port(word: u32) -> u16 {
    let [high, low, ..] = word.to_ne_bytes();
    u16::from_be_bytes([high, low])
}

#[cfg(target_os = "windows")]
fn listener_table(family: u32) -> anyhow::Result<Vec<u32>> {
    const ATTEMPTS: usize = 4;
    let mut table: Vec<u32> = Vec::new();
    let mut size = 0_u32;
    for _ in 0..ATTEMPTS {
        let status = unsafe {
            GetExtendedTcpTable(
                table.as_mut_ptr().cast(),
                &raw mut size,
                FALSE,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        match status {
            NO_ERROR => return Ok(table),
            ERROR_INSUFFICIENT_BUFFER => {
                table.resize(usize::try_from(size)?.div_ceil(size_of::<u32>()), 0);
            },
            error => return Err(io::Error::from_raw_os_error(error.cast_signed()).into()),
        }
    }
    anyhow::bail!("The system's listener table kept changing")
}

#[cfg(target_os = "windows")]
fn listener_owners<const WORDS: usize>(
    table: &[u32],
    port: u16,
    row: fn(&[u32; WORDS]) -> Row,
) -> Vec<u32> {
    let Some((&count, rows)) = table.split_first() else {
        return Vec::new();
    };
    rows.as_chunks::<WORDS>()
        .0
        .iter()
        .take(usize::try_from(count).unwrap_or(usize::MAX))
        .map(row)
        .filter(|listener| listener.port == port && reaches_loopback(listener.address))
        .map(|listener| listener.owner)
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

    pub fn leader_exited(&self) -> io::Result<bool> {
        self.leader.map_or(Ok(true), leader_exited)
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
    leader_exited(pid).is_ok()
}

#[cfg(unix)]
fn leader_exited(pid: libc::pid_t) -> io::Result<bool> {
    loop {
        let mut info = MaybeUninit::<libc::siginfo_t>::zeroed();
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid.cast_unsigned(),
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(unsafe { info.assume_init().si_pid() } == pid);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
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
    pub fn loopback_listener(&self, port: u16) -> anyhow::Result<LoopbackListener> {
        classify(
            || loopback_listener_inodes(port),
            |inodes| self.unheld(inodes),
        )
    }

    fn unheld(&self, mut inodes: Vec<u64>) -> anyhow::Result<Vec<u64>> {
        let Some(leader) = self.leader.filter(|&leader| unreaped(leader)) else {
            return Ok(inodes);
        };

        release_held(leader, &mut inodes);
        if !inodes.is_empty() {
            for member in fs::read_dir("/proc")?
                .flatten()
                .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
                .filter(|&pid| process_group(pid) == Some(leader))
            {
                release_held(member, &mut inodes);
            }
        }
        Ok(inodes)
    }
}

#[cfg(target_os = "linux")]
fn loopback_listener_inodes(port: u16) -> anyhow::Result<Vec<u64>> {
    let mut inodes = loopback_listeners(&fs::read_to_string("/proc/net/tcp")?, port);
    match fs::read_to_string("/proc/net/tcp6") {
        Ok(table) => inodes.extend(loopback_listeners(&table, port)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    Ok(inodes)
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
                && reaches_loopback(kernel_address(address)?)
                && u16::from_str_radix(local_port, 16).ok()? == port;
            listening.then_some(inode)?.parse().ok()
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn kernel_address(hex: &str) -> Option<IpAddr> {
    let word = |digits: &[u8; 8]| u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok();
    match hex.as_bytes().as_chunks::<8>() {
        ([ipv4], []) => Some(IpAddr::from(word(ipv4)?.to_ne_bytes())),
        ([a, b, c, d], []) => Some(IpAddr::V6(ipv6([word(a)?, word(b)?, word(c)?, word(d)?]))),
        _ => None,
    }
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
mod tests;
