use super::*;
use crate::testing::PATIENCE;

fn next_request(requests: &Receiver<Event>) -> anyhow::Result<Request> {
    let started = Instant::now();
    loop {
        if let Ok(event) = requests.try_recv() {
            return match event {
                Event::Legacy(request) => Ok(request),
                Event::Control(_) => bail!("Unexpected control call"),
            };
        }
        ensure!(
            started.elapsed() < PATIENCE,
            "Instance request was not delivered"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn occupy_server_with_pending_requests(instance: &Instance, path: &Path) -> anyhow::Result<()> {
    Instance::send(path, Request::Toggle)?;
    Instance::send(path, Request::Cancel)?;
    let endpoint = Endpoint::read(&path.with_file_name(PORT_FILE))?;
    let mut next = TcpStream::connect_timeout(&instance.address, PATIENCE)?;
    next.set_read_timeout(Some(IO_TIMEOUT))?;
    next.write_all(&endpoint.nonce.to_le_bytes())?;
    assert!(next.read_exact(&mut [0]).is_err());
    Ok(())
}

#[test]
fn relaunch_requests_settings_and_shutdown_releases_the_lock() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    assert!(Instance::acquire(&path)?.is_none());
    assert_eq!(next_request(&requests)?, Request::Reveal);
    for request in [Request::Toggle, Request::Cancel] {
        Instance::send(&path, request)?;
        assert_eq!(next_request(&requests)?, request);
    }
    drop(instance);
    assert!(requests.is_closed());
    assert!(!directory.path().join(PORT_FILE).exists());
    assert!(Instance::acquire(&path)?.is_some());
    Ok(())
}

#[test]
fn requests_without_the_secret_are_never_delivered() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let endpoint = Endpoint::read(&path.with_file_name(PORT_FILE))?;
    let mut forged = TcpStream::connect_timeout(&instance.address, PATIENCE)?;
    forged.set_read_timeout(Some(PATIENCE))?;
    forged.write_all(&(endpoint.nonce ^ 1).to_le_bytes())?;
    forged.write_all(&[Request::Cancel as u8])?;
    assert!(
        forged.read_exact(&mut [0]).is_err(),
        "A wrong secret was acknowledged"
    );
    Instance::send(&path, Request::Toggle)?;
    assert_eq!(next_request(&requests)?, Request::Toggle);
    Ok(())
}

#[test]
fn a_secret_trickled_past_the_timeout_is_never_acknowledged() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (instance, _requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let endpoint = Endpoint::read(&path.with_file_name(PORT_FILE))?;
    let mut slow = TcpStream::connect_timeout(&instance.address, PATIENCE)?;

    slow.set_nodelay(true)?;
    slow.set_read_timeout(Some(PATIENCE))?;

    for byte in endpoint.nonce.to_le_bytes() {
        if slow.write_all(&[byte]).is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        slow.read_exact(&mut [0]).is_err(),
        "A trickled secret held the server past its timeout"
    );
    Ok(())
}

#[test]
fn unreachable_instances_name_their_remedy() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let stale = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port();
    for (published, remedy) in [
        (format!("{stale} {:032x}", 1), NOT_RUNNING),
        (stale.to_string(), OUTDATED),
    ] {
        fs::write(directory.path().join(PORT_FILE), published)?;
        let error = Instance::send(&path, Request::Toggle)
            .err()
            .context("An unreachable instance accepted a request")?;
        assert!(format!("{error:#}").starts_with(remedy), "{error:#}");
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn only_the_owner_can_read_the_published_secret() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let _owner = Instance::acquire(&path)?.context("First owner missing")?;
    let mode = fs::metadata(directory.path().join(PORT_FILE))?
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    Ok(())
}

#[test]
fn pending_commands_wait_for_the_ui_instead_of_disappearing() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    occupy_server_with_pending_requests(&instance, &path)?;
    for expected in [Request::Toggle, Request::Cancel] {
        assert_eq!(next_request(&requests)?, expected);
    }
    drop(instance);
    assert!(requests.is_closed());
    Ok(())
}

#[test]
fn failed_accepts_are_retried_until_shutdown() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let (Endpoint { nonce, .. }, _port_file) =
        Endpoint::publish(listener.local_addr()?.port(), directory.path())?;
    let (failure, failures) = std::sync::mpsc::channel();
    let mut reaches_listener = [false, true].into_iter();
    let accept = move || {
        if reaches_listener.next() == Some(true) {
            return listener.accept().map(|(stream, _)| stream);
        }
        failure.send(()).unwrap();
        Err(io::ErrorKind::ConnectionAborted.into())
    };
    let (requests, received) = async_channel::bounded(1);
    let started = Instant::now();
    let server = OwnedThread::spawn("instance-server", {
        let requests = requests.clone();
        move || serve(accept, nonce, &requests)
    })?;

    let delivered = Instance::send(&path, Request::Toggle).and_then(|()| next_request(&received));
    thread::sleep(ACCEPT_RETRY * 5);
    requests.close();
    let (finished, completion) = std::sync::mpsc::channel();
    let cleanup = thread::spawn(move || {
        drop(server);
        finished.send(()).unwrap();
    });
    let stopped = completion.recv_timeout(PATIENCE);
    let elapsed = started.elapsed();
    assert_eq!(delivered?, Request::Toggle);
    stopped.context("A server whose accepts kept failing outlived shutdown")?;

    let most = elapsed.as_millis() / ACCEPT_RETRY.as_millis() + 2;
    let retried = failures.try_iter().count();
    assert!(
        retried <= usize::try_from(most)?,
        "{retried} failed accepts in {elapsed:?}"
    );
    cleanup.join().unwrap();
    Ok(())
}

#[test]
fn shutdown_wakes_a_server_waiting_for_the_ui() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (instance, _requests) = Instance::acquire(&path)?.context("First owner missing")?;
    occupy_server_with_pending_requests(&instance, &path)?;
    let (finished, completion) = std::sync::mpsc::channel();
    let cleanup = thread::spawn(move || {
        drop(instance);
        finished.send(()).unwrap();
    });
    completion.recv_timeout(PATIENCE)?;
    cleanup.join().unwrap();
    assert!(Instance::acquire(&path)?.is_some());
    Ok(())
}

mod control;
