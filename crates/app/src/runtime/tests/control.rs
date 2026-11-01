use anyhow::Context as _;

use super::*;
use crate::{
    control::{Call, Command, Disposition, Reply},
    transcript::style::Case,
};

async fn control(h: &Harness, command: Command) -> anyhow::Result<Reply> {
    let (call, receipt) = Call::new(command);
    h.runtime.control(call);
    receive(&receipt).await
}

async fn ready(h: &mut Harness) -> anyhow::Result<()> {
    receive(&h.loads).await?;
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await
}

#[tokio::test(start_paused = true)]
async fn start_admits_once_with_its_own_style_and_stop_is_session_bound() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    ready(&mut h).await?;
    let started = control(
        &h,
        Command::Start {
            style: Some(Style::Identifier(Case::Snake)),
        },
    )
    .await?;
    assert_eq!(started.result, Disposition::Ok);
    let token = started
        .session
        .context("Start did not identify its session")?;
    receive(&h.captures).await?;
    assert_eq!(
        control(&h, Command::Start { style: None }).await?.result,
        Disposition::Refused
    );
    assert_eq!(
        control(
            &h,
            Command::Stop {
                session: "0000000000000000-1".into()
            }
        )
        .await?
        .result,
        Disposition::Refused
    );
    assert!(h.jobs.try_recv().is_err());
    assert_eq!(
        control(
            &h,
            Command::Stop {
                session: token.clone()
            }
        )
        .await?
        .result,
        Disposition::Ok
    );
    let (_, reply) = receive(&h.jobs).await?;
    assert_eq!(
        control(&h, Command::Stop { session: token }).await?.result,
        Disposition::Ok
    );
    answer(reply, "HTTP fixture name.")?;
    assert_eq!(receive(&h.pasted).await?, "http_fixture_name");
    h.phase(Phase::Done).await?;
    assert!(h.captures.try_recv().is_err());
    assert!(h.jobs.try_recv().is_err());
    assert_eq!(*h.styles.borrow(), Style::Prose);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn stale_cancel_cannot_revoke_a_newer_insertion_and_current_cancel_replies_after_revoke()
-> anyhow::Result<()> {
    let (insertion, preparing) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| ports.insertion = Some(insertion))?;
    ready(&mut h).await?;
    h.start().await?;
    let first = control(&h, Command::Status)
        .await?
        .session
        .context("No first token")?;
    h.finish_as("First fixture.").await?;
    preparing.reached().await?;
    preparing.release().await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    h.start().await?;
    let current = control(&h, Command::Status)
        .await?
        .session
        .context("No current token")?;
    h.finish_as("Second fixture.").await?;
    let permit = preparing.reached().await?;
    assert_eq!(
        control(&h, Command::Cancel { session: first })
            .await?
            .result,
        Disposition::Refused
    );
    assert!(permit.active());
    assert_eq!(
        control(&h, Command::Cancel { session: current })
            .await?
            .result,
        Disposition::Ok
    );
    assert!(!permit.active());
    preparing.release().await?;
    h.phase(Phase::Cancelled).await?;
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn disconnected_callers_cannot_start_and_tokens_do_not_survive_a_new_owner()
-> anyhow::Result<()> {
    let mut first = Harness::new()?;
    ready(&mut first).await?;
    let old = control(&first, Command::Start { style: None })
        .await?
        .session
        .context("No old token")?;
    first.close().await?;
    let mut h = Harness::new()?;
    ready(&mut h).await?;
    let (call, receipt) = Call::new(Command::Start { style: None });
    drop(receipt);
    h.runtime.control(call);
    assert!(control(&h, Command::Status).await?.session.is_none());
    assert!(h.captures.try_recv().is_err());
    let new = control(&h, Command::Start { style: None })
        .await?
        .session
        .context("No new token")?;
    assert_ne!(old, new);
    assert_eq!(
        control(&h, Command::Cancel { session: old }).await?.result,
        Disposition::Refused
    );
    assert_eq!(control(&h, Command::Status).await?.session, Some(new));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn ordinary_control_cannot_finish_a_show_or_start_over_practice() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    ready(&mut h).await?;
    h.start().await?;
    let token = control(&h, Command::Status)
        .await?
        .session
        .context("No token before promotion")?;
    h.input(Input::Show(Scope::Screen));
    receive(&h.shows).await?;
    assert!(control(&h, Command::Status).await?.session.is_none());
    assert_eq!(
        control(&h, Command::Cancel { session: token })
            .await?
            .result,
        Disposition::Refused
    );
    assert_eq!(
        control(&h, Command::PasteLast).await?.result,
        Disposition::Refused
    );
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    let _practice = arm_practice(&h).await?;
    assert_eq!(
        control(&h, Command::Start { style: None }).await?.result,
        Disposition::Refused
    );
    h.close().await
}

mod presets;

#[tokio::test(start_paused = true)]
async fn replay_does_not_advertise_capture_control_authority() -> anyhow::Result<()> {
    let (insertion, preparing) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| ports.insertion = Some(insertion))?;
    ready(&mut h).await?;
    let old = control(&h, Command::Start { style: None })
        .await?
        .session
        .context("No recording token")?;
    receive(&h.captures).await?;
    control(
        &h,
        Command::Stop {
            session: old.clone(),
        },
    )
    .await?;
    let (_, reply) = receive(&h.jobs).await?;
    answer(reply, "Replay fixture.")?;
    preparing.reached().await?;
    preparing.release().await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    let accepted = control(&h, Command::PasteLast).await?;
    assert_eq!(accepted.result, Disposition::Ok);
    assert!(accepted.session.is_none());
    let permit = preparing.reached().await?;
    assert!(control(&h, Command::Status).await?.session.is_none());
    assert_eq!(
        control(&h, Command::Stop { session: old }).await?.result,
        Disposition::Refused
    );
    assert!(permit.active());
    preparing.release().await?;
    assert_eq!(receive(&h.pasted).await?, "Replay fixture.");
    h.phase(Phase::Done).await?;
    assert!(h.captures.try_recv().is_err());
    h.close().await
}
