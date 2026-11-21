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
fn a_flood_of_idle_connections_neither_locks_out_nor_holds_back_shortcuts() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let idle = (0..MAX_HANDSHAKES * 4)
        .map(|_| dial(&path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    for request in [Request::Toggle, Request::Cancel] {
        Instance::send(&path, request)?;
    }
    assert_eq!(next_request(&requests)?, Request::Toggle);
    assert_eq!(next_request(&requests)?, Request::Cancel);
    let mut latest: &TcpStream = &idle.last().context("No idle connection")?.0;
    latest.set_nonblocking(true)?;
    assert!(
        matches!(latest.read(&mut [0]), Err(error) if error.kind() == io::ErrorKind::WouldBlock),
        "Shortcuts waited for idle connections to time out"
    );
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
    thread::sleep(IO_TIMEOUT / 10);
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

fn deliverable(arrivals: &Arrivals) -> Option<Request> {
    match arrivals.next(Instant::now()) {
        Next::Deliver(_, request) => Some(request),
        Next::HoldUntil(_) | Next::Hold => None,
    }
}

async fn read_by_server(
    arrivals: &mut Arrivals,
    listener: &tokio::net::TcpListener,
    nonce: u128,
    request: Request,
) -> anyhow::Result<(
    TcpStream,
    Result<(Id, Option<(AsyncStream, Incoming)>), JoinError>,
)> {
    let mut caller = TcpStream::connect(listener.local_addr()?)?;
    present(&mut caller, nonce, &[request as u8])?;
    arrivals.begin(listener.accept().await?.0, nonce);
    let presented = arrivals
        .verifying
        .join_next_with_id()
        .await
        .context("The secret was never checked")?;
    arrivals.verified(presented);
    let read = arrivals
        .reading
        .join_next_with_id()
        .await
        .context("The request was never read")?;
    Ok((caller, read))
}

#[tokio::test]
async fn a_shortcut_whose_caller_has_visibly_left_is_not_admitted() -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let nonce = 7;
    let mut arrivals = Arrivals::default();
    let (departed, read) = read_by_server(&mut arrivals, &listener, nonce, Request::Toggle).await?;
    drop(departed);
    let Ok((id, Some((stream, incoming)))) = read else {
        bail!("The departed caller's request was not read");
    };
    assert_eq!(stream.peek(&mut [0]).await?, 0);
    assert!(
        arrivals
            .finish(Ok((id, Some((stream, incoming)))), 0)
            .await
            .is_none()
    );
    assert_eq!(deliverable(&arrivals), None);
    let (staying, read) = read_by_server(&mut arrivals, &listener, nonce, Request::Cancel).await?;
    assert!(arrivals.finish(read, 0).await.is_none());
    assert_eq!(deliverable(&arrivals), Some(Request::Cancel));
    assert!(acknowledged(&staying)?);
    Ok(())
}

#[tokio::test]
async fn an_unchecked_connection_holds_later_shortcuts_only_for_its_grace() -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let mut arrivals = Arrivals::default();
    let accepted = Instant::now();
    let unchecked = arrivals
        .verifying
        .spawn(future::pending::<Option<AsyncStream>>());
    arrivals.order.push_back(Arrival {
        accepted,
        stage: Stage::Unverified(unchecked),
    });
    let (_caller, read) = read_by_server(&mut arrivals, &listener, 7, Request::Toggle).await?;
    assert!(arrivals.finish(read, 0).await.is_none());
    let expiry = accepted
        .checked_add(ORDER_GRACE)
        .context("The grace overflows")?;
    assert!(matches!(arrivals.next(accepted), Next::HoldUntil(until) if until == expiry));
    assert!(matches!(
        arrivals.next(expiry),
        Next::Deliver(1, Request::Toggle)
    ));
    Ok(())
}

fn hold_slot(arrivals: &mut Arrivals, presented: bool) {
    let stage = if presented {
        Stage::Reading(arrivals.reading.spawn(future::pending()))
    } else {
        Stage::Unverified(arrivals.verifying.spawn(future::pending()))
    };
    arrivals.order.push_back(Arrival {
        accepted: Instant::now(),
        stage,
    });
}

#[tokio::test]
async fn connections_still_sending_their_request_stay_within_the_bound() -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let mut arrivals = Arrivals::default();
    hold_slot(&mut arrivals, false);
    for _ in 1..MAX_HANDSHAKES {
        hold_slot(&mut arrivals, true);
    }
    let _displacing = TcpStream::connect(listener.local_addr()?)?;
    arrivals.begin(listener.accept().await?.0, 7);
    assert_eq!(arrivals.order.len(), MAX_HANDSHAKES);
    let unchecked: Vec<usize> = arrivals
        .order
        .iter()
        .enumerate()
        .filter(|(_, arrival)| matches!(arrival.stage, Stage::Unverified(_)))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(unchecked, [MAX_HANDSHAKES - 1]);
    let mut full = Arrivals::default();
    for _ in 0..MAX_HANDSHAKES {
        hold_slot(&mut full, true);
    }
    let mut refused = TcpStream::connect(listener.local_addr()?)?;
    full.begin(listener.accept().await?.0, 7);
    assert_eq!(full.order.len(), MAX_HANDSHAKES);
    assert!(full.verifying.is_empty());
    refused.set_read_timeout(Some(PATIENCE))?;
    assert_eq!(refused.read(&mut [0])?, 0);
    Ok(())
}
