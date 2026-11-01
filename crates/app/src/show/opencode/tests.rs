use std::{fs, path::Path, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use super::*;

mod server;
use server::{Server, Step};

const PATIENCE: Duration = Duration::from_secs(5);
const SESSION: &str = "ses_explicit";

fn row(id: &str) -> Value {
    json!({
        "id": id,
        "title": "PUBLIC_SESSION_TITLE",
        "location": {"directory": "/public/project"},
        "time": {"updated": 1000},
    })
}

fn health() -> Step {
    Step::reply("/api/info", json!({"pid": 42, "version": "public-test"}))
}

fn selected() -> Step {
    Step::reply("/api/session/ses_explicit", json!({"data": row(SESSION)}))
}

fn admitted(session: &str) -> Value {
    json!({"data": {
        "id": "msg_admitted", "sessionID": session, "type": "user", "delivery": "queue",
        "payload": {"text": "PRIVATE_REPLY_MUST_NOT_APPEAR"},
        "time": {"created": 1000},
    }})
}

fn show() -> anyhow::Result<tempfile::TempDir> {
    let directory = tempfile::tempdir()?;
    fs::create_dir(directory.path().join("frames"))?;
    let frames: Vec<_> = (1_u64..=7)
        .map(|index| json!({"index": index, "at": index.saturating_mul(100), "reason": "middle", "marks": [0, 0]}))
        .collect();
    let manifest = serde_json::to_vec(&json!({
        "version": 2, "duration": 1000, "surface": {"width": 100, "height": 100},
        "strokes": [], "pins": [], "actions": [], "narration": {"text": "NOT_SENT_NARRATION"},
        "marks": [], "keyframes": frames,
    }))?;
    let parsed = speakeasy_show::Manifest::parse(&manifest)?;
    fs::write(directory.path().join("show.json"), manifest)?;
    fs::write(directory.path().join("summary.md"), "PUBLIC_EDITED_SUMMARY")?;
    let image =
        speakeasy_show::Image::from_bgra(2, 2, [20, 30, 40, 255].repeat(4))?.encode_png()?;
    for (name, _) in parsed.sheets() {
        fs::write(directory.path().join(name), &image)?;
    }
    for name in ["voice.wav", "movie.mp4", "recovery.json"] {
        fs::write(directory.path().join(name), "NOT_SENT_PRIVATE_EXTRA")?;
    }
    Ok(directory)
}

fn request(directory: &Path, endpoint: &str) -> Request {
    Request {
        directory: directory.into(),
        session: SESSION.into(),
        expected_endpoint: Some(endpoint.into()),
    }
}

#[tokio::test]
async fn explicit_send_preserves_edits_all_sheets_and_auth_and_checks_receipt() -> anyhow::Result<()>
{
    let directory = show()?;
    let server = Server::start(vec![
        health(),
        selected(),
        Step::reply("/api/session/ses_explicit/prompt", admitted(SESSION)),
    ])
    .await?;
    let mut deliveries = Deliveries::default();
    let receipt = deliveries.send_from(
        request(directory.path(), &server.endpoint),
        Ok(server.registration.clone()),
    )?;
    let outcome = tokio::time::timeout(PATIENCE, receipt).await?;
    let Outcome::Queued {
        session,
        message,
        attachments,
        bytes,
    } = outcome
    else {
        anyhow::bail!("The valid fixture was not queued")
    };
    assert_eq!(
        (session.as_str(), message.as_str(), attachments),
        (SESSION, "msg_admitted", 2)
    );
    let calls = server.finish().await?;
    assert_eq!(calls.len(), 3);
    for call in &calls {
        let authorization = call.headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim())
        });
        assert_eq!(
            authorization,
            Some(format!("Basic {}", STANDARD.encode("opencode:PUBLIC_TEST_PASSWORD")).as_str())
        );
    }
    let call = calls.last().unwrap();
    assert_eq!(bytes, call.body.len());
    let body: Value = serde_json::from_slice(&call.body)?;
    assert_eq!(body["delivery"], "queue");
    assert!(body.get("id").is_none());
    assert!(
        body["text"]
            .as_str()
            .unwrap()
            .contains("PUBLIC_EDITED_SUMMARY")
    );
    assert!(body["text"].as_str().unwrap().contains("## Edited summary"));
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    for file in files {
        let name = file["name"].as_str().unwrap();
        let encoded = file["uri"]
            .as_str()
            .unwrap()
            .strip_prefix("data:image/png;base64,")
            .unwrap();
        assert_eq!(
            STANDARD.decode(encoded)?,
            fs::read(directory.path().join(name))?
        );
    }
    let serialized = String::from_utf8(call.body.clone())?;
    assert!(!serialized.contains("NOT_SENT_"));
    Ok(())
}

#[tokio::test]
async fn missing_session_or_endpoint_drift_never_posts() -> anyhow::Result<()> {
    for changed_endpoint in [false, true] {
        let directory = show()?;
        let mut steps = vec![health()];
        if !changed_endpoint {
            steps.push(Step::rejection(
                "/api/session/ses_explicit",
                "404 Not Found",
            ));
        }
        let server = Server::start(steps).await?;
        let mut selected = request(directory.path(), &server.endpoint);
        if changed_endpoint {
            selected.expected_endpoint = Some("http://127.0.0.1:1/".into());
        }
        let mut deliveries = Deliveries::default();
        let receipt = deliveries.send_from(selected, Ok(server.registration.clone()))?;
        assert!(matches!(
            tokio::time::timeout(PATIENCE, receipt).await?,
            Outcome::NotSubmitted { .. }
        ));
        let calls = server.finish().await?;
        assert!(calls.iter().all(|call| !call.headers.starts_with("POST ")));
    }
    Ok(())
}

#[tokio::test]
async fn wrong_receipt_rejection_and_dropped_connection_are_honest_and_never_retried()
-> anyhow::Result<()> {
    let mut malformed = admitted(SESSION);
    malformed["data"]["delivery"] = json!("steer");
    for (reply, rejected) in [
        (
            Step::reply("/api/session/ses_explicit/prompt", admitted("ses_other")),
            false,
        ),
        (
            Step::reply("/api/session/ses_explicit/prompt", malformed),
            false,
        ),
        (Step::close("/api/session/ses_explicit/prompt"), false),
        (
            Step::rejection("/api/session/ses_explicit/prompt", "403 Forbidden"),
            true,
        ),
    ] {
        let directory = show()?;
        let server = Server::start(vec![health(), selected(), reply]).await?;
        let mut deliveries = Deliveries::default();
        let receipt = deliveries.send_from(
            request(directory.path(), &server.endpoint),
            Ok(server.registration.clone()),
        )?;
        let outcome = tokio::time::timeout(PATIENCE, receipt).await?;
        match outcome {
            Outcome::NotSubmitted { reason } if rejected => assert!(!reason.contains("PRIVATE")),
            Outcome::Unknown { reason } if !rejected => {
                assert!(reason.contains("Check the session before sending again"));
                assert!(!reason.contains("PRIVATE"));
            },
            _ => anyhow::bail!("The fixture reported an incorrect admission outcome"),
        }
        let calls = server.finish().await?;
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.headers.starts_with("POST "))
                .count(),
            1
        );
    }
    Ok(())
}

#[tokio::test]
async fn cancelled_preflight_sends_nothing_and_remains_owned_until_exit() -> anyhow::Result<()> {
    let directory = show()?;
    let (blocked, reached) = async_channel::bounded(1);
    let (release, gate) = async_channel::bounded(1);
    let server = Server::start(vec![
        health(),
        Step::close("/api/session/ses_explicit").held(blocked, gate),
    ])
    .await?;
    let mut deliveries = Deliveries::default();
    let receipt = deliveries.send_from(
        request(directory.path(), &server.endpoint),
        Ok(server.registration.clone()),
    )?;
    tokio::time::timeout(PATIENCE, reached.recv()).await??;
    deliveries.cancel();
    assert!(matches!(
        tokio::time::timeout(PATIENCE, receipt).await?,
        Outcome::NotSubmitted { .. }
    ));
    tokio::time::timeout(PATIENCE, deliveries.drained()).await?;
    release.send(()).await?;
    let calls = server.finish().await?;
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| !call.headers.starts_with("POST ")));
    Ok(())
}

#[tokio::test]
async fn committed_posts_drain_despite_cancel_or_closed_view() -> anyhow::Result<()> {
    for closed_view in [false, true] {
        let directory = show()?;
        let (blocked, reached) = async_channel::bounded(1);
        let (release, gate) = async_channel::bounded(1);
        let server = Server::start(vec![
            health(),
            selected(),
            Step::reply("/api/session/ses_explicit/prompt", admitted(SESSION)).held(blocked, gate),
        ])
        .await?;
        let mut deliveries = Deliveries::default();
        let mut receipt = Some(deliveries.send_from(
            request(directory.path(), &server.endpoint),
            Ok(server.registration.clone()),
        )?);
        tokio::time::timeout(PATIENCE, reached.recv()).await??;
        if closed_view {
            receipt = None;
        } else {
            deliveries.cancel();
        }
        assert!(deliveries.is_busy());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), deliveries.drained())
                .await
                .is_err()
        );
        assert!(
            deliveries
                .send_from(
                    request(directory.path(), &server.endpoint),
                    Ok(server.registration.clone())
                )
                .is_err()
        );
        release.send(()).await?;
        tokio::time::timeout(PATIENCE, deliveries.drained()).await?;
        if let Some(receipt) = receipt {
            assert!(matches!(receipt.await, Outcome::Queued { .. }));
        }
        assert_eq!(server.finish().await?.len(), 3);
        assert!(!deliveries.is_busy());
    }
    Ok(())
}

#[tokio::test]
async fn abandoned_lookup_cancels_and_does_not_block_a_later_request() -> anyhow::Result<()> {
    let (blocked, reached) = async_channel::bounded(1);
    let (release, gate) = async_channel::bounded(1);
    let server = Server::start(vec![Step::close("/api/info").held(blocked, gate)]).await?;
    let mut deliveries = Deliveries::default();
    let receipt = deliveries.list_from(Ok(server.registration.clone()))?;
    tokio::time::timeout(PATIENCE, reached.recv()).await??;
    drop(receipt);
    tokio::time::timeout(PATIENCE, deliveries.drained()).await?;
    release.send(()).await?;
    assert_eq!(server.finish().await?.len(), 1);
    let receipt = deliveries.list_from(Err("PUBLIC_PREFLIGHT_REFUSAL"))?;
    assert_eq!(
        receipt.await.err().as_deref(),
        Some("PUBLIC_PREFLIGHT_REFUSAL")
    );
    Ok(())
}

#[tokio::test]
async fn listing_bounds_pages_and_returns_rows_without_selecting_one() -> anyhow::Result<()> {
    let mut steps = vec![health()];
    for page in 0_u32..5 {
        let query = if page == 0 {
            "/api/session?parentID=null&archived=false&limit=100".into()
        } else {
            format!("/api/session?parentID=null&archived=false&limit=100&cursor={page}")
        };
        steps.push(Step::reply(query, json!({"data": [row(&format!("ses_{page}"))], "cursor": {"next": page.saturating_add(1).to_string()}})));
    }
    let server = Server::start(steps).await?;
    let mut deliveries = Deliveries::default();
    let receipt = deliveries.list_from(Ok(server.registration.clone()))?;
    let listing = tokio::time::timeout(PATIENCE, receipt)
        .await?
        .map_err(anyhow::Error::msg)?;
    assert_eq!(listing.endpoint, server.endpoint);
    assert_eq!(listing.sessions.len(), 5);
    assert!(listing.truncated);
    assert_eq!(listing.sessions[0].id, "ses_0");
    assert_eq!(listing.sessions[0].directory, "/public/project");
    assert_eq!(listing.sessions[0].updated, 1000);
    assert_eq!(server.finish().await?.len(), 6);
    Ok(())
}

#[test]
fn malformed_missing_and_oversized_sheets_refuse_complete_payload() -> anyhow::Result<()> {
    let (_stop, stopping) = async_channel::bounded(1);
    let (_interest, interested) = async_channel::bounded(1);
    let cancel = Cancellation {
        stopping,
        interested,
    };
    for fault in 0..3 {
        let directory = show()?;
        let sheet = directory.path().join("contact-sheet-02.png");
        match fault {
            0 => fs::remove_file(sheet)?,
            1 => fs::write(sheet, b"PRIVATE_INVALID_PNG")?,
            _ => fs::write(sheet, vec![0; 9 * 1024 * 1024 + 1])?,
        }
        let reason = payload::prepare(directory.path(), &cancel).err().unwrap();
        assert!(!reason.contains("PRIVATE_INVALID"));
        if fault == 2 {
            assert!(reason.contains("12 MiB"));
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn linked_sheet_is_refused() -> anyhow::Result<()> {
    let directory = show()?;
    let target = directory.path().join("contact-sheet.png");
    fs::remove_file(&target)?;
    std::os::unix::fs::symlink(directory.path().join("contact-sheet-02.png"), &target)?;
    let (_stop, stopping) = async_channel::bounded(1);
    let (_interest, interested) = async_channel::bounded(1);
    assert!(
        payload::prepare(
            directory.path(),
            &Cancellation {
                stopping,
                interested
            }
        )
        .is_err()
    );
    Ok(())
}
