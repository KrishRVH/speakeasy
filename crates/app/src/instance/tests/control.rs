use super::*;
use crate::{
    control::{Command, Disposition, MAX_BYTES, Reply, Status},
    transcript::style::Style,
};

fn status() -> Status {
    Status {
        version: env!("CARGO_PKG_VERSION").into(),
        service: "running".into(),
        phase: "idle".into(),
        model: "ready".into(),
        desktop_ready: true,
        session: None,
        style: Style::Prose,
        preset: None,
        last_dictation: false,
    }
}

#[test]
fn control_waits_for_its_owner_and_preserves_the_reply() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let client = thread::spawn(move || Instance::control(&path, Command::Start { style: None }));
    let Event::Control(call) = requests.recv_blocking()? else {
        panic!("Control request became a shortcut action");
    };
    assert!(
        !client.is_finished(),
        "Admission was reported as completion"
    );
    assert!(call.is_live());
    let mut accepted = status();
    accepted.session = Some("0123456789abcdef-7".into());
    call.respond(Reply::ok(accepted));
    let reply = client.join().unwrap();
    assert_eq!(reply.result, Disposition::Ok);
    assert_eq!(reply.session.as_deref(), Some("0123456789abcdef-7"));
    Ok(())
}

#[test]
fn pending_control_does_not_block_shortcut_cancel_or_toggle() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let mut client = connect(&path.with_file_name(PORT_FILE))?;
    client.write_all(&[CONTROL])?;
    write_frame(&mut client, &Command::Status.encode().unwrap())?;
    let Event::Control(call) = requests.recv_blocking()? else {
        panic!("Control request became a shortcut action");
    };
    for request in [Request::Cancel, Request::Toggle] {
        Instance::send(&path, request)?;
        assert_eq!(next_request(&requests)?, request);
    }
    assert!(call.is_live());
    call.respond(Reply::ok(status()));
    let reply = Reply::decode(&read_frame(&mut client)?).unwrap();
    assert_eq!(reply.result, Disposition::Ok);
    Ok(())
}

#[test]
fn concurrent_control_is_bounded_and_shutdown_closes_every_receipt() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let mut clients = Vec::new();
    let mut calls = Vec::new();
    for _ in 0..MAX_CONTROL_CALLS {
        let mut client = connect(&path.with_file_name(PORT_FILE))?;
        client.write_all(&[CONTROL])?;
        write_frame(&mut client, &Command::Status.encode().unwrap())?;
        let Event::Control(call) = requests.recv_blocking()? else {
            panic!("Control request became a shortcut action");
        };
        clients.push(client);
        calls.push(call);
    }
    let overflow = Instance::control(&path, Command::Status);
    assert_eq!(overflow.result, Disposition::Refused);
    assert!(requests.try_recv().is_err());
    Instance::send(&path, Request::Cancel)?;
    assert_eq!(next_request(&requests)?, Request::Cancel);
    drop(instance);
    for call in calls {
        assert!(call.reply.is_closed());
        assert!(!call.is_live());
    }
    for mut client in clients {
        assert!(read_frame(&mut client).is_err());
    }
    Ok(())
}

#[test]
fn a_full_control_lane_refuses_without_waiting_or_dropping_shortcut_work() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    Instance::send(&path, Request::Toggle)?;
    let reply = Instance::control(&path, Command::Status);
    assert_eq!(reply.result, Disposition::Refused);
    assert_eq!(next_request(&requests)?, Request::Toggle);
    assert!(requests.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn a_disconnected_caller_closes_its_pending_owner_receipt() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let mut client = connect(&path.with_file_name(PORT_FILE))?;
    client.write_all(&[CONTROL])?;
    write_frame(
        &mut client,
        &Command::Start { style: None }.encode().unwrap(),
    )?;
    let Event::Control(call) = tokio::time::timeout(PATIENCE, requests.recv()).await?? else {
        panic!("Control request became a shortcut action");
    };
    drop(client);
    tokio::time::timeout(PATIENCE, call.reply.closed()).await?;
    assert!(!call.is_live());
    Ok(())
}

#[test]
fn unconfirmed_calls_expire_without_being_retried() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let client = thread::spawn(move || Instance::control(&path, Command::Start { style: None }));
    let Event::Control(call) = requests.recv_blocking()? else {
        panic!("Control request became a shortcut action");
    };
    let reply = client.join().unwrap();
    assert_eq!(reply.result, Disposition::Unknown);
    assert_eq!(reply.reason.as_deref(), Some(UNCONFIRMED));
    assert!(!call.is_live());
    assert!(requests.try_recv().is_err());
    Ok(())
}

#[test]
fn an_abandoned_receipt_is_unknown_without_claiming_a_disconnection() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let client = thread::spawn(move || Instance::control(&path, Command::Start { style: None }));
    let Event::Control(call) = requests.recv_blocking()? else {
        panic!("Control request became a shortcut action");
    };
    drop(call);
    let reply = client.join().unwrap();
    assert_eq!(reply.result, Disposition::Unknown);
    assert_eq!(reply.reason.as_deref(), Some(UNCONFIRMED));
    assert!(requests.try_recv().is_err());
    Ok(())
}

#[test]
fn malformed_versions_and_unknown_fields_never_reach_the_owner() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    for json in [
        r#"{"v":2,"op":"start"}"#,
        r#"{"v":1,"op":"start","text":"private fixture"}"#,
        "[1,2]",
    ] {
        let mut client = connect(&path.with_file_name(PORT_FILE))?;
        client.write_all(&[CONTROL])?;
        write_frame(&mut client, json.as_bytes())?;
        let reply = Reply::decode(&read_frame(&mut client)?).unwrap();
        assert_eq!(reply.result, Disposition::Refused);
        assert!(!reply.reason.unwrap().contains("private fixture"));
        assert!(requests.try_recv().is_err());
    }
    Ok(())
}

#[test]
fn oversized_control_frames_are_refused_before_reading_the_body() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let mut client = connect(&path.with_file_name(PORT_FILE))?;
    client.write_all(&[CONTROL])?;
    let oversized = u16::try_from(MAX_BYTES)?.saturating_add(1);
    client.write_all(&oversized.to_be_bytes())?;
    let reply = Reply::decode(&read_frame(&mut client)?).unwrap();
    assert_eq!(reply.result, Disposition::Refused);
    assert!(requests.try_recv().is_err());
    Instance::send(&path, Request::Toggle)?;
    assert_eq!(next_request(&requests)?, Request::Toggle);
    Ok(())
}

#[test]
fn a_peer_closing_after_the_control_opcode_is_never_reported_as_success() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let (endpoint, _port_file) =
        Endpoint::publish(listener.local_addr()?.port(), directory.path())?;
    let server = thread::spawn(move || -> anyhow::Result<()> {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(PATIENCE))?;
        let mut nonce = [0; 16];
        stream.read_exact(&mut nonce)?;
        assert_eq!(u128::from_le_bytes(nonce), endpoint.nonce);
        stream.write_all(&[ACKNOWLEDGED])?;
        let mut opcode = [0];
        stream.read_exact(&mut opcode)?;
        assert_eq!(opcode, [CONTROL]);
        Ok(())
    });
    let reply = Instance::control(&path, Command::PasteLast);
    assert_eq!(reply.result, Disposition::Unknown);
    assert!(reply.reason.unwrap().contains("Restart Speakeasy"));
    server.join().unwrap()?;
    Ok(())
}
