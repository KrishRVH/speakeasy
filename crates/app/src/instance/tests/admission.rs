use super::*;

fn present(stream: &mut TcpStream, nonce: u128, request: &[u8]) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.write_all(&[nonce.to_le_bytes().as_slice(), request].concat())
}

fn acknowledged(mut stream: &TcpStream) -> io::Result<bool> {
    stream.set_read_timeout(Some(PATIENCE))?;
    let mut reply = [0];
    stream.read_exact(&mut reply)?;
    Ok(reply == [ACKNOWLEDGED])
}

#[test]
fn a_flood_of_idle_connections_never_locks_out_a_shortcut() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let idle = (0..MAX_HANDSHAKES * 4)
        .map(|_| dial(&path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Instance::send(&path, Request::Toggle)?;
    assert_eq!(next_request(&requests)?, Request::Toggle);
    drop(idle);
    Ok(())
}

#[tokio::test]
async fn a_shortcut_queued_ahead_of_a_burst_is_read_before_the_burst_displaces_it()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let address = listener.local_addr()?;
    let (Endpoint { nonce, .. }, _port_file) = Endpoint::publish(address.port(), directory.path())?;
    let mut first = TcpStream::connect(address)?;
    present(&mut first, nonce, &[Request::Toggle as u8])?;
    let burst = (0..MAX_HANDSHAKES * 4)
        .map(|_| TcpStream::connect(address))
        .collect::<io::Result<Vec<_>>>()?;
    let (requests, received) = async_channel::bounded(1);
    let server_requests = requests.clone();
    let server = tokio::spawn(async move {
        accept_connections(
            || async { listener.accept().await.map(|(stream, _)| stream) },
            nonce,
            &server_requests,
        )
        .await;
    });
    let reply = tokio::task::spawn_blocking(move || acknowledged(&first)).await?;
    assert!(reply?, "The first caller was not acknowledged");
    let delivered = tokio::time::timeout(PATIENCE, received.recv()).await??;
    assert!(matches!(delivered, Event::Shortcut(Request::Toggle)));
    drop(burst);
    requests.close();
    tokio::time::timeout(PATIENCE, server).await??;
    Ok(())
}

#[test]
fn shortcuts_reach_the_ui_in_connection_order() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let (mut toggle, nonce) = dial(&path)?;
    present(&mut toggle, nonce, &[])?;
    Instance::send(&path, Request::Cancel)?;
    thread::sleep(IO_TIMEOUT / 4);
    assert!(
        requests.try_recv().is_err(),
        "A later shortcut overtook a caller still sending its request"
    );
    toggle.write_all(&[Request::Toggle as u8])?;
    assert!(
        acknowledged(&toggle)?,
        "The earlier caller was not acknowledged"
    );
    assert_eq!(next_request(&requests)?, Request::Toggle);
    assert_eq!(next_request(&requests)?, Request::Cancel);
    Ok(())
}

#[test]
fn concurrent_shortcuts_beyond_the_waiting_limit_are_refused_and_never_delivered()
-> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let sent = [Request::Toggle, Request::Cancel, Request::Reveal];
    let mut callers = sent
        .iter()
        .map(|_| dial(&path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    for ((caller, nonce), request) in callers.iter_mut().zip(sent) {
        present(caller, *nonce, &[request as u8])?;
    }
    let admitted: Vec<_> = callers
        .iter()
        .zip(sent)
        .filter(|((caller, _), _)| acknowledged(caller).unwrap_or(false))
        .map(|(_, request)| request)
        .collect();
    assert_eq!(admitted.len(), 2, "{admitted:?}");
    for expected in admitted {
        assert_eq!(next_request(&requests)?, expected);
    }
    thread::sleep(IO_TIMEOUT);
    assert!(
        requests.try_recv().is_err(),
        "A refused shortcut was delivered"
    );
    Ok(())
}
