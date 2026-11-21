use anyhow::Context as _;

use super::*;
use crate::control::{Call, Command, Disposition, Reply};

fn scripted() -> anyhow::Result<Harness> {
    Harness::with_ports(
        Config {
            print_to_scripts: true,
            ..Config::default()
        },
        |_| {},
    )
}

async fn captured(h: &mut Harness) -> anyhow::Result<String> {
    let started = h.call(Command::Capture { style: None }).await?;
    assert_eq!(started.result, Disposition::Ok);
    receive(&h.captures).await?;
    h.observe(|snapshot| snapshot.for_script).await?;
    started
        .session
        .context("Capture did not identify its session")
}

impl Harness {
    async fn call(&self, command: Command) -> anyhow::Result<Reply> {
        let (call, receipt) = Call::new(command);
        self.runtime.control(call);
        receive(&receipt).await
    }

    fn print(&self, session: String) -> async_channel::Receiver<Reply> {
        let (call, receipt) = Call::new(Command::Print { session });
        self.runtime.control(call);
        receipt
    }
}

#[tokio::test(start_paused = true)]
async fn a_captured_dictation_answers_its_caller_and_pastes_nothing() -> anyhow::Result<()> {
    let mut h = scripted()?;
    h.ready().await?;
    let token = captured(&mut h).await?;
    let printed = h.print(token);
    h.transcribe_next_as("Fixture words.").await?;
    let reply = receive(&printed).await?;
    assert_eq!(reply.result, Disposition::Ok);
    assert_eq!(
        reply.text.as_deref().map(String::as_str),
        Some("Fixture words.")
    );
    h.phase(Phase::Done).await?;
    assert!(h.pasted.try_recv().is_err());
    assert!(h.entered.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn only_a_captured_dictation_can_be_printed() -> anyhow::Result<()> {
    let mut h = scripted()?;
    h.ready().await?;
    let started = h.call(Command::Start { style: None }).await?;
    receive(&h.captures).await?;
    let token = started
        .session
        .context("Start did not identify its session")?;
    let refused = receive(&h.print(token.clone())).await?;
    assert_eq!(refused.result, Disposition::Refused);
    assert!(refused.text.is_none());
    assert_eq!(
        h.call(Command::Stop { session: token }).await?.result,
        Disposition::Ok
    );
    h.transcribe_next_as("Typed fixture.").await?;
    assert_eq!(receive(&h.pasted).await?, "Typed fixture.");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_capture_that_ends_without_words_refuses_its_caller() -> anyhow::Result<()> {
    let mut h = scripted()?;
    h.ready().await?;
    let token = captured(&mut h).await?;
    let printed = h.print(token);
    h.transcribe_next_as("").await?;
    let reply = receive(&printed).await?;
    assert_eq!(reply.result, Disposition::Refused);
    assert!(reply.text.is_none());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_capture_stopped_without_print_keeps_its_words_off_the_screen() -> anyhow::Result<()> {
    let mut h = scripted()?;
    h.ready().await?;
    let token = captured(&mut h).await?;
    assert_eq!(
        h.call(Command::Stop { session: token }).await?.result,
        Disposition::Ok
    );
    h.transcribe_next_as("Private fixture.").await?;
    h.phase(Phase::Error).await?;
    assert!(h.pasted.try_recv().is_err());
    assert!(h.snapshot().last_dictation);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_capture_refuses_its_waiting_caller() -> anyhow::Result<()> {
    let mut h = scripted()?;
    h.ready().await?;
    let token = captured(&mut h).await?;
    let printed = h.print(token.clone());
    let (_, job) = receive(&h.jobs).await?;
    assert_eq!(
        h.call(Command::Cancel { session: token }).await?.result,
        Disposition::Ok
    );
    let reply = receive(&printed).await?;
    assert_eq!(reply.result, Disposition::Refused);
    assert!(reply.text.is_none());
    drop(job);
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_second_print_is_refused_while_the_first_still_gets_the_words() -> anyhow::Result<()> {
    let mut h = scripted()?;
    h.ready().await?;
    let token = captured(&mut h).await?;
    let first = h.print(token.clone());
    let (_, job) = receive(&h.jobs).await?;
    let second = receive(&h.print(token)).await?;
    assert_eq!(second.result, Disposition::Refused);
    assert!(second.text.is_none());
    answer(job, "Fixture words.")?;
    assert_eq!(
        receive(&first).await?.text.as_deref().map(String::as_str),
        Some("Fixture words.")
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn print_is_refused_until_the_user_turns_it_on() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.ready().await?;
    let refused = h.call(Command::Capture { style: None }).await?;
    assert_eq!(refused.result, Disposition::Refused);
    assert!(h.captures.try_recv().is_err());
    let started = h.call(Command::Start { style: None }).await?;
    receive(&h.captures).await?;
    let token = started
        .session
        .context("Start did not identify its session")?;
    let printed = receive(&h.print(token)).await?;
    assert_eq!(printed.result, Disposition::Refused);
    assert!(!h.snapshot().for_script);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn turning_print_off_during_a_capture_ends_it_and_refuses_its_caller() -> anyhow::Result<()> {
    let mut h = scripted()?;
    h.ready().await?;
    let token = captured(&mut h).await?;
    let mut config = h.runtime.configuration.borrow().clone();
    config.print_to_scripts = false;
    h.runtime.configure(ValidConfig::assume(config));
    h.observe(|snapshot| !snapshot.phase.is_active()).await?;
    let printed = receive(&h.print(token)).await?;
    assert_eq!(printed.result, Disposition::Refused);
    assert!(printed.text.is_none());
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}
