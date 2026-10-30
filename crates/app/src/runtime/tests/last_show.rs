//! The last saved Show: which publications the owner remembers, which it never does, and that the
//! memory survives a failed copy, a failed or abandoned Show, a later dictation, and a new owner.

use super::*;

/// The folder the fakes save a Show that said `said` in.
fn saved(said: &str) -> SavedShow {
    SavedShow::assume(format!("shows/{said}"))
}

/// Records a Show that says `said` and finishes it, up to its publication.
async fn record_show(h: &mut Harness, said: &str) -> anyhow::Result<()> {
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    receive(&h.shows).await?;
    for input in [Input::Release, Input::Press, Input::Release] {
        h.input(input);
    }
    h.transcribe_next_as(said).await
}

/// Records a Show that says `said`, returning once the fake has saved it.
async fn save_show(h: &mut Harness, said: &str) -> anyhow::Result<()> {
    record_show(h, said).await?;
    receive(&h.published).await?;
    Ok(())
}

/// A settings change, which abandons whatever session is under way.
fn change_settings(h: &Harness) {
    h.runtime.configure(ValidConfig::assume(Config {
        remove_fillers: false,
        ..Config::default()
    }));
}

#[tokio::test(start_paused = true)]
async fn a_show_saved_but_not_copied_is_remembered_and_says_how_to_copy_it() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.copy_error = Some("Another app holds the clipboard");
    })?;
    save_show(&mut h, "look").await?;
    h.phase(Phase::Error).await?;
    let snapshot = h.snapshot();
    assert_eq!(snapshot.last_show, Some(saved("look")));
    assert_eq!(
        snapshot.message,
        "The Show was saved but not copied (Another app holds the clipboard). Choose Copy last \
         Show in Speakeasy's menu."
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_that_fails_or_is_abandoned_before_it_is_saved_leaves_the_last_one()
-> anyhow::Result<()> {
    let (publication, publishing) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publication = Some(publication);
    })?;
    record_show(&mut h, "first").await?;
    publishing.reached().await?;
    publishing.release().await?;
    receive(&h.published).await?;
    h.phase(Phase::Copied).await?;

    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    let (id, _, events) = receive(&h.shows).await?;
    events
        .send(ShowEvent::Finished(
            id,
            Err(anyhow!("Screen recording is off")),
        ))
        .await?;
    h.phase(Phase::Error).await?;
    assert_eq!(
        h.snapshot().last_show,
        Some(saved("first")),
        "A failure forgot it"
    );
    h.input(Input::Release);

    record_show(&mut h, "abandoned").await?;
    publishing.reached().await?;
    change_settings(&h);
    h.phase(Phase::Cancelled).await?;
    publishing.release().await?;
    assert!(
        still_pending(h.published.recv()).await,
        "An abandoned Show was saved"
    );
    assert_eq!(
        h.snapshot().last_show,
        Some(saved("first")),
        "Abandoning forgot it"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_newer_show_replaces_the_last_one_and_dictation_leaves_it() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    save_show(&mut h, "first").await?;
    h.phase(Phase::Copied).await?;
    assert_eq!(h.snapshot().last_show, Some(saved("first")));
    save_show(&mut h, "second").await?;
    h.observe(|snapshot| snapshot.last_show == Some(saved("second")))
        .await?;
    h.start().await?;
    h.finish_as("dictated").await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    assert_eq!(h.snapshot().last_show, Some(saved("second")));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_abandoned_after_it_is_saved_changes_neither_the_last_show_nor_the_outcome()
-> anyhow::Result<()> {
    let (saving, persisted) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.saved = Some(saving);
    })?;
    save_show(&mut h, "first").await?;
    persisted.reached().await?;
    persisted.release().await?;
    h.phase(Phase::Copied).await?;
    save_show(&mut h, "second").await?;
    persisted.reached().await?;
    change_settings(&h);
    h.phase(Phase::Cancelled).await?;
    persisted.release().await?;
    assert!(
        still_pending(h.observe(|snapshot| {
            snapshot.last_show != Some(saved("first")) || snapshot.phase != Phase::Cancelled
        }))
        .await,
        "The abandoned Show's result reached the owner"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_new_owner_starts_from_the_last_show_presented() -> anyhow::Result<()> {
    let (output, mut updates) = watch::channel(Snapshot {
        epoch: SnapshotEpoch::default().next(),
        last_show: Some(saved("before a pause")),
        ..Snapshot::default()
    });
    let (meter, _) = watch::channel(Meter::default());
    let (ports, reports) = FakePorts::new();
    let mut runtime = Runtime::on_task(Config::default(), output, meter, ports)?;
    receive(&reports.loads).await?;
    timeout(
        PATIENCE,
        updates.wait_for(|snapshot| snapshot.model == ModelState::Ready),
    )
    .await??;
    assert_eq!(updates.borrow().last_show, Some(saved("before a pause")));
    runtime.close().await
}

#[tokio::test(start_paused = true)]
async fn the_last_show_copied_again_reads_as_a_copied_show_only_while_idle() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.report(Report::ShowCopied)?;
    // The report lane outranks capture, so the level arrives only after the report is handled.
    events.send(CaptureEvent::Level(id, 0.5)).await?;
    h.observe_level(0.5).await?;
    assert_eq!(h.snapshot().phase, Phase::Recording);
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.report(Report::ShowCopied)?;
    h.phase(Phase::Copied).await?;
    assert_eq!(h.snapshot().message, SHOW_COPIED);
    h.close().await
}
