use anyhow::Context as _;

use super::*;
use crate::{
    control::{Call, Command, Disposition, Reply},
    transcript::style::Case,
};

async fn finish_in(h: &mut Harness, language: &str) -> anyhow::Result<()> {
    h.input(Input::Toggle);
    let (heard, reply) = receive(&h.jobs).await?;
    assert_eq!(heard, language);
    answer(reply, "Fixture.")?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await
}

async fn control(h: &Harness, command: Command) -> anyhow::Result<Reply> {
    let (call, receipt) = Call::new(command);
    h.runtime.control(call);
    receive(&receipt).await
}

#[tokio::test(start_paused = true)]
async fn start_admits_once_with_its_own_style_and_stop_is_session_bound() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.ready().await?;
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
async fn disconnected_callers_cannot_start_and_tokens_do_not_survive_a_new_owner()
-> anyhow::Result<()> {
    let mut first = Harness::new()?;
    first.ready().await?;
    let old = control(&first, Command::Start { style: None })
        .await?
        .session
        .context("No old token")?;
    first.close().await?;
    let mut h = Harness::new()?;
    h.ready().await?;
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
    h.ready().await?;
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

mod configuration;
mod presets;
