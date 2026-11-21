use std::net::{Ipv4Addr, TcpListener, TcpStream};

use anyhow::Context as _;

use super::{LoopbackListener, ProcessGroup};

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
    let counted = group.loopback_listener(port);
    group.terminate();
    Command::new("kill").arg(stranger.to_string()).status()?;
    let ended = reports.next().context("No status")??;
    parent.wait()?;
    assert_eq!(counted?, LoopbackListener::Foreign);
    assert_eq!(ended, "143", "Ended by SIGTERM, not SIGKILL");
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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
    assert_eq!(group.loopback_listener(port)?, LoopbackListener::Owned);
    assert_eq!(
        group.loopback_listener(foreign.local_addr()?.port())?,
        LoopbackListener::Foreign
    );
    assert_eq!(
        group.loopback_listener(client.local_addr()?.port())?,
        LoopbackListener::Missing
    );
    group.terminate();
    child.wait()?;
    assert_ne!(group.loopback_listener(port)?, LoopbackListener::Owned);
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn listeners_held_by_members_other_than_the_leader_count() -> anyhow::Result<()> {
    use std::{
        io::{BufRead as _, BufReader},
        os::{fd::OwnedFd, unix::process::CommandExt as _},
        process::{Command, Stdio},
    };

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
    assert_eq!(group.loopback_listener(port)?, LoopbackListener::Owned);
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
    assert_eq!(group.loopback_listener(held)?, LoopbackListener::Owned);
    assert_ne!(group.loopback_listener(port)?, LoopbackListener::Owned);

    drop(group);
    child.wait()?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[test]
fn only_listeners_that_outlive_the_ownership_scan_are_foreign() {
    use super::classify;

    const HELD: u32 = 1;
    for (listings, expected) in [
        (vec![vec![HELD]], Some(LoopbackListener::Owned)),
        (vec![vec![7], vec![7]], Some(LoopbackListener::Foreign)),
        (vec![vec![7], vec![]], Some(LoopbackListener::Missing)),
        (
            vec![vec![7, HELD], vec![HELD]],
            Some(LoopbackListener::Owned),
        ),
        ((2..=6).map(|listener| vec![listener]).collect(), None),
    ] {
        let mut remaining = listings.clone().into_iter();
        let classified = classify(
            || remaining.next().context("Listed too often"),
            |mut listeners| {
                listeners.retain(|&listener| listener != HELD);
                Ok(listeners)
            },
        );
        assert_eq!(classified.ok(), expected, "{listings:?}");
    }
}

#[test]
fn only_a_listener_reached_twice_outside_the_group_is_foreign() {
    use super::classify_reachable;

    for (held, reached, expected) in [
        (vec![true], vec![], LoopbackListener::Owned),
        (vec![false], vec![false], LoopbackListener::Missing),
        (vec![false, true], vec![true], LoopbackListener::Owned),
        (
            vec![false, false],
            vec![true, false],
            LoopbackListener::Missing,
        ),
        (
            vec![false, false, true],
            vec![true, true],
            LoopbackListener::Owned,
        ),
        (
            vec![false, false, false],
            vec![true, true],
            LoopbackListener::Foreign,
        ),
    ] {
        let (mut checks, mut connects) = (held.clone().into_iter(), reached.clone().into_iter());
        let classified = classify_reachable(
            || Ok(checks.next().expect("Checked the group too often")),
            || Ok(connects.next().expect("Connected too often")),
        );
        assert_eq!(classified.ok(), Some(expected), "{held:?} {reached:?}");
        assert!(
            checks.next().is_none() && connects.next().is_none(),
            "Stopped early: {held:?} {reached:?}"
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn native_words(address: std::net::Ipv6Addr) -> [u32; 4] {
    let octets = address.octets();
    let &[a, b, c, d] = octets.as_chunks::<4>().0 else {
        panic!("An IPv6 address has four quads");
    };
    [a, b, c, d].map(u32::from_ne_bytes)
}

#[cfg(target_os = "linux")]
#[test]
fn every_listener_a_loopback_connection_reaches_counts() {
    use std::net::{IpAddr, Ipv6Addr};

    const PORT: u16 = 8080;
    let table = |rows: &[(IpAddr, u16, &str, u64)]| {
        let rows = rows.iter().map(|&(address, port, state, inode)| {
            let local = match address {
                IpAddr::V4(address) => format!("{:08X}", u32::from_ne_bytes(address.octets())),
                IpAddr::V6(address) => native_words(address)
                    .map(|word| format!("{word:08X}"))
                    .concat(),
            };
            format!("0: {local}:{port:04X} 0:0 {state} 0:0 0:0 0 1000 0 {inode} 1 0 100 0 0 10 0")
        });
        std::iter::once("  sl  local_address rem_address   st tx_queue rx_queue".to_owned())
            .chain(rows)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let ipv4 = table(&[
        (Ipv4Addr::LOCALHOST.into(), PORT, "0A", 1),
        (Ipv4Addr::UNSPECIFIED.into(), PORT, "0A", 2),
        (Ipv4Addr::new(127, 0, 0, 2).into(), PORT, "0A", 3),
        (Ipv4Addr::LOCALHOST.into(), 8081, "0A", 4),
        (Ipv4Addr::LOCALHOST.into(), PORT, "01", 5),
    ]);
    let ipv6 = table(&[
        (Ipv6Addr::UNSPECIFIED.into(), PORT, "0A", 6),
        (Ipv4Addr::LOCALHOST.to_ipv6_mapped().into(), PORT, "0A", 7),
        (Ipv6Addr::LOCALHOST.into(), PORT, "0A", 8),
        (
            Ipv4Addr::new(127, 0, 0, 2).to_ipv6_mapped().into(),
            PORT,
            "0A",
            9,
        ),
        (
            Ipv4Addr::UNSPECIFIED.to_ipv6_mapped().into(),
            PORT,
            "0A",
            10,
        ),
    ]);
    assert_eq!(super::loopback_listeners(&ipv4, PORT), [1, 2]);
    assert_eq!(super::loopback_listeners(&ipv6, PORT), [6, 7, 10]);
}

#[cfg(target_os = "windows")]
#[test]
fn every_listener_a_loopback_connection_reaches_counts() {
    use std::net::Ipv6Addr;

    use super::{IPV4_ROW_WORDS, IPV6_ROW_WORDS, ipv4_row, ipv6_row, listener_owners};

    const PORT: u16 = 8080;
    const LISTEN: u32 = 2;
    let port = |port: u16| {
        let [high, low] = port.to_be_bytes();
        u32::from_ne_bytes([high, low, 0, 0])
    };
    let ipv4 = [
        (Ipv4Addr::LOCALHOST, PORT, 11),
        (Ipv4Addr::UNSPECIFIED, PORT, 12),
        (Ipv4Addr::new(127, 0, 0, 2), PORT, 13),
        (Ipv4Addr::LOCALHOST, 8081, 14),
    ]
    .into_iter()
    .flat_map(|(address, local, owner)| -> [u32; IPV4_ROW_WORDS] {
        let address = u32::from_ne_bytes(address.octets());
        [LISTEN, address, port(local), 0, 0, owner]
    });
    let ipv6 = [
        (Ipv6Addr::UNSPECIFIED, PORT, 21),
        (Ipv4Addr::LOCALHOST.to_ipv6_mapped(), PORT, 22),
        (Ipv6Addr::LOCALHOST, PORT, 23),
        (Ipv4Addr::UNSPECIFIED.to_ipv6_mapped(), PORT, 24),
    ]
    .into_iter()
    .flat_map(|(address, local, owner)| -> [u32; IPV6_ROW_WORDS] {
        let [a, b, c, d] = native_words(address);
        [a, b, c, d, 0, port(local), 0, 0, 0, 0, 0, 0, LISTEN, owner]
    });
    assert_eq!(
        listener_owners(
            &std::iter::once(4).chain(ipv4).collect::<Vec<_>>(),
            PORT,
            ipv4_row
        ),
        [11, 12]
    );
    assert_eq!(
        listener_owners(
            &std::iter::once(4).chain(ipv6).collect::<Vec<_>>(),
            PORT,
            ipv6_row
        ),
        [21, 22, 24]
    );
}
