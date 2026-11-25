use super::*;
use crate::{control::Disposition, testing::PATIENCE};

fn next_event(requests: &Receiver<Event>) -> anyhow::Result<Event> {
    let started = std::time::Instant::now();
    loop {
        if let Ok(event) = requests.try_recv() {
            return Ok(event);
        }
        anyhow::ensure!(
            started.elapsed() < PATIENCE,
            "Instance request wasn't delivered"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn next_request(requests: &Receiver<Event>) -> anyhow::Result<Request> {
    match next_event(requests)? {
        Event::Shortcut(request) => Ok(request),
        Event::Control(_) => bail!("Unexpected control call"),
    }
}

fn next_call(requests: &Receiver<Event>) -> anyhow::Result<Call> {
    match next_event(requests)? {
        Event::Control(call) => Ok(call),
        Event::Shortcut(_) => bail!("Control request became a shortcut action"),
    }
}

#[tokio::test]
async fn relaunch_reveals_the_owner_and_quit_releases_the_lock() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    assert!(Instance::acquire(&path)?.is_none());
    assert_eq!(next_request(&requests)?, Request::Reveal);
    Instance::send(&path, Request::Toggle)?;
    assert_eq!(next_request(&requests)?, Request::Toggle);
    for request in [Request::Cancel, Request::Toggle] {
        Instance::send(&path, request)?;
    }
    tokio::time::timeout(PATIENCE, instance.close()).await?;
    assert!(requests.is_closed());
    assert!(!directory.path().join(PORT_FILE).exists());
    assert!(Instance::acquire(&path)?.is_some());
    Ok(())
}

#[test]
fn a_command_waits_for_its_owner_while_shortcuts_keep_flowing() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let client = thread::spawn({
        let path = path.clone();
        move || Instance::control(&path, Command::PasteLast)
    });
    let call = next_call(&requests)?;
    assert_eq!(call.command, Command::PasteLast);
    Instance::send(&path, Request::Cancel)?;
    assert_eq!(next_request(&requests)?, Request::Cancel);
    assert!(
        !client.is_finished(),
        "The caller didn't wait for its owner"
    );
    call.respond(Reply::refused("Fixture reason."));
    let reply = client.join().unwrap();
    assert_eq!(reply.result, Disposition::Refused);
    assert_eq!(reply.reason.as_deref(), Some("Fixture reason."));
    Ok(())
}

#[test]
fn a_command_the_owner_drops_is_unknown() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("settings.json");
    let (_instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
    let client = thread::spawn(move || Instance::control(&path, Command::Status));
    drop(next_call(&requests)?);
    let reply = client.join().unwrap();
    assert_eq!(reply.result, Disposition::Unknown);
    assert_eq!(reply.reason.as_deref(), Some(UNCONFIRMED));
    Ok(())
}
