use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
#[cfg(target_os = "macos")]
use std::{ffi::c_int, io, mem::MaybeUninit};

use super::reaches_loopback;

#[cfg(target_os = "macos")]
const PROC_PIDFDSOCKETINFO: c_int = 3;
#[cfg(target_os = "macos")]
const SOCKINFO_TCP: c_int = 2;
const TSI_S_LISTEN: i32 = 1;
const INI_IPV4: u8 = 0x1;
const INI_IPV6: u8 = 0x2;

#[cfg(target_os = "macos")]
#[repr(C)]
struct SocketFdInfo {
    _pfi: [u64; 3],
    psi: SocketInfo,
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct SocketInfo {
    _soi_stat: libc::vinfo_stat,
    _soi_so: u64,
    _soi_pcb: u64,
    _soi_type: c_int,
    _soi_protocol: c_int,
    _soi_family: c_int,
    _soi_options: i16,
    _soi_linger: i16,
    _soi_state: i16,
    _soi_qlen: i16,
    _soi_incqlen: i16,
    _soi_qlimit: i16,
    _soi_timeo: i16,
    _soi_error: u16,
    _soi_oobmark: u32,
    _soi_rcv: [u32; 6],
    _soi_snd: [u32; 6],
    soi_kind: c_int,
    _rfu_1: u32,
    soi_proto: SocketProto,
}

#[cfg(target_os = "macos")]
#[repr(C)]
union SocketProto {
    pri_tcp: TcpSockInfo,
    _pri_un: [u64; 66],
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct TcpSockInfo {
    tcpsi_ini: InSockInfo,
    tcpsi_state: i32,
    _tcpsi_timer: [i32; 4],
    _tcpsi_mss: i32,
    _tcpsi_flags: u32,
    _rfu_1: u32,
    _tcpsi_tp: u64,
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct InSockInfo {
    _insi_fport: i32,
    insi_lport: i32,
    _insi_gencnt: u64,
    _insi_flags: u32,
    _insi_flow: u32,
    insi_vflag: u8,
    _insi_ip_ttl: u8,
    _rfu_1: u32,
    _insi_faddr: [u8; 16],
    insi_laddr: [u8; 16],
    _insi_v4: u8,
    _insi_v6: [i32; 3],
}

const _: () = {
    use std::mem::offset_of;
    assert!(size_of::<InSockInfo>() == 80);
    assert!(offset_of!(InSockInfo, insi_lport) == 4);
    assert!(offset_of!(InSockInfo, insi_vflag) == 24);
    assert!(offset_of!(InSockInfo, insi_laddr) == 48);
    assert!(offset_of!(InSockInfo, _insi_v6) == 68);
    assert!(size_of::<TcpSockInfo>() == 120);
    assert!(offset_of!(TcpSockInfo, tcpsi_state) == 80);
    assert!(offset_of!(TcpSockInfo, _tcpsi_tp) == 112);
};

#[cfg(target_os = "macos")]
const _: () = {
    use std::mem::offset_of;
    assert!(size_of::<libc::vinfo_stat>() == 136);
    assert!(size_of::<SocketProto>() == 528);
    assert!(offset_of!(SocketInfo, _soi_rcv) == 184);
    assert!(offset_of!(SocketInfo, soi_kind) == 232);
    assert!(offset_of!(SocketInfo, soi_proto) == 240);
    assert!(size_of::<SocketInfo>() == 768);
    assert!(offset_of!(SocketFdInfo, psi) == 24);
    assert!(size_of::<SocketFdInfo>() == 792);
};

#[cfg(target_os = "macos")]
pub(super) fn group_listens(leader: libc::pid_t, port: u16) -> io::Result<bool> {
    for member in members(leader)? {
        for descriptor in descriptors(member)? {
            if descriptor.proc_fdtype == libc::PROX_FDTYPE_SOCKET.cast_unsigned()
                && tcp_socket(member, descriptor.proc_fd)?
                    .is_some_and(|socket| listens(&socket, port))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
fn members(leader: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    listed(0, |members, bytes| {
        let count = unsafe { libc::proc_listpgrppids(leader, members.as_mut_ptr().cast(), bytes) };
        usize::try_from(count).unwrap_or(0)
    })
}

#[cfg(target_os = "macos")]
fn descriptors(pid: libc::pid_t) -> io::Result<Vec<libc::proc_fdinfo>> {
    let unused = libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    };
    listed(unused, |descriptors, bytes| {
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                descriptors.as_mut_ptr().cast(),
                bytes,
            )
        };
        usize::try_from(written / libc::PROC_PIDLISTFD_SIZE).unwrap_or(0)
    })
}

#[cfg(target_os = "macos")]
fn listed<T: Copy>(
    unused: T,
    mut fill: impl FnMut(&mut [T], c_int) -> usize,
) -> io::Result<Vec<T>> {
    for capacity in [64, 512, 4096] {
        let mut entries = vec![unused; capacity];
        let bytes = c_int::try_from(size_of_val(entries.as_slice())).map_err(io::Error::other)?;
        let filled = fill(&mut entries, bytes);
        if filled < capacity {
            entries.truncate(filled);
            return Ok(entries);
        }
    }
    Err(io::Error::other(
        "The engine's group listed too many entries",
    ))
}

#[cfg(target_os = "macos")]
fn tcp_socket(pid: libc::pid_t, descriptor: c_int) -> io::Result<Option<TcpSockInfo>> {
    let size = c_int::try_from(size_of::<SocketFdInfo>()).map_err(io::Error::other)?;
    let mut socket = MaybeUninit::<SocketFdInfo>::zeroed();
    let written = unsafe {
        libc::proc_pidfdinfo(
            pid,
            descriptor,
            PROC_PIDFDSOCKETINFO,
            socket.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::EBADF | libc::ENOTSOCK | libc::ESRCH) => Ok(None),
            _ => Err(error),
        };
    }
    let socket = unsafe { socket.assume_init() }.psi;
    Ok((socket.soi_kind == SOCKINFO_TCP).then_some(unsafe { socket.soi_proto.pri_tcp }))
}

fn listens(socket: &TcpSockInfo, port: u16) -> bool {
    let local = &socket.tcpsi_ini;
    socket.tcpsi_state == TSI_S_LISTEN
        && u16::try_from(local.insi_lport).is_ok_and(|bound| u16::from_be(bound) == port)
        && ipv4_reachable_address(local).is_some_and(reaches_loopback)
}

fn ipv4_reachable_address(socket: &InSockInfo) -> Option<IpAddr> {
    if socket.insi_vflag & INI_IPV4 == 0 {
        return None;
    }
    if socket.insi_vflag & INI_IPV6 != 0 {
        return Some(IpAddr::V6(Ipv6Addr::from(socket.insi_laddr)));
    }
    let [.., a, b, c, d] = socket.insi_laddr;
    Some(IpAddr::V4(Ipv4Addr::new(a, b, c, d)))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::{INI_IPV4, INI_IPV6, InSockInfo, TSI_S_LISTEN, TcpSockInfo, listens};

    #[test]
    fn every_listener_a_loopback_connection_reaches_counts() {
        const PORT: u16 = 8080;
        const TSI_S_ESTABLISHED: i32 = 4;
        const DUAL: u8 = INI_IPV4 | INI_IPV6;
        let socket = |state, vflag, address: IpAddr, port: u16| TcpSockInfo {
            tcpsi_ini: InSockInfo {
                insi_lport: i32::from(port.to_be()),
                insi_vflag: vflag,
                insi_laddr: match address {
                    IpAddr::V4(address) => address.to_ipv6_compatible().octets(),
                    IpAddr::V6(address) => address.octets(),
                },
                ..InSockInfo::default()
            },
            tcpsi_state: state,
            ..TcpSockInfo::default()
        };
        let [localhost, other, any] = [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(127, 0, 0, 2),
            Ipv4Addr::UNSPECIFIED,
        ];
        let [loopback6, any6] = [Ipv6Addr::LOCALHOST, Ipv6Addr::UNSPECIFIED];
        let mapped = |address: Ipv4Addr| IpAddr::V6(address.to_ipv6_mapped());
        let sockets: [(u8, i32, u8, IpAddr, u16); 11] = [
            (1, TSI_S_LISTEN, INI_IPV4, localhost.into(), PORT),
            (2, TSI_S_LISTEN, INI_IPV4, any.into(), PORT),
            (3, TSI_S_LISTEN, INI_IPV4, other.into(), PORT),
            (4, TSI_S_LISTEN, INI_IPV4, localhost.into(), 8081),
            (5, TSI_S_ESTABLISHED, INI_IPV4, localhost.into(), PORT),
            (6, TSI_S_LISTEN, DUAL, any6.into(), PORT),
            (7, TSI_S_LISTEN, DUAL, mapped(localhost), PORT),
            (8, TSI_S_LISTEN, INI_IPV6, loopback6.into(), PORT),
            (9, TSI_S_LISTEN, DUAL, mapped(other), PORT),
            (10, TSI_S_LISTEN, DUAL, mapped(any), PORT),
            (11, TSI_S_LISTEN, INI_IPV6, any6.into(), PORT),
        ];
        let counted: Vec<u8> = sockets
            .into_iter()
            .filter(|&(_, state, vflag, address, port)| {
                listens(&socket(state, vflag, address, port), PORT)
            })
            .map(|(id, ..)| id)
            .collect();
        assert_eq!(counted, [1, 2, 6, 7, 10]);
    }
}
