//! The last saved Show: which publications the owner remembers, which it never does, that the
//! memory survives a failed copy, a failed or cancelled Show, a later dictation, a pause, and a new
//! owner, that a Show saved as its session is abandoned still counts, and how the menu's reports
//! about it show and forget it.

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
async fn a_show_that_fails_before_it_is_saved_leaves_the_last_one() -> anyhow::Result<()> {
    let mut h = Harness::remembering(saved("first"), |ports| {
        ports.publish_error = Some("Disk full");
    })?;
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
        "A failed recording forgot it"
    );
    h.input(Input::Release);

    record_show(&mut h, "unsaved").await?;
    h.observe(|snapshot| snapshot.message.contains("Disk full"))
        .await?;
    assert_eq!(
        h.snapshot().last_show,
        Some(saved("first")),
        "A publication that failed before its commit forgot it"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_cancelled_before_it_is_saved_leaves_the_last_one() -> anyhow::Result<()> {
    let (publication, publishing) = gate(1);
    let mut h = Harness::remembering(saved("first"), |ports| {
        ports.publication = Some(publication);
    })?;
    record_show(&mut h, "cancelled").await?;
    publishing.reached().await?;
    // Pausing revokes even a saving Show's permit before the owner hears of it, so the
    // publication itself reports the cancellation.
    h.runtime.input.cancel();
    publishing.release().await?;
    h.phase(Phase::Cancelled).await?;
    assert_eq!(h.snapshot().last_show, Some(saved("first")));
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
async fn a_show_saved_as_it_is_abandoned_is_remembered_but_still_reads_as_cancelled()
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
    // Committed before the settings change abandons it, so its summary is copied regardless.
    persisted.reached().await?;
    change_settings(&h);
    h.phase(Phase::Cancelled).await?;
    persisted.release().await?;
    h.observe(|snapshot| snapshot.last_show == Some(saved("second")))
        .await?;
    assert_eq!(
        h.snapshot().phase,
        Phase::Cancelled,
        "The abandoned Show's result replaced the cancel"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_saved_after_a_pause_announces_still_outlives_its_owner() -> anyhow::Result<()> {
    let (saving, persisted) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.saved = Some(saving);
    })?;
    save_show(&mut h, "newer").await?;
    persisted.reached().await?;
    // A pause announces before it stops the owner, which is still finishing this Show.
    h.output
        .send_modify(|snapshot| snapshot.epoch = snapshot.epoch.next());
    persisted.release().await?;
    h.observe(|snapshot| snapshot.last_show == Some(saved("newer")))
        .await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_saved_as_a_pause_stops_its_owner_outlives_that_owner() -> anyhow::Result<()> {
    let (saving, persisted) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.saved = Some(saving);
    })?;
    save_show(&mut h, "newer").await?;
    persisted.reached().await?;
    // A pause announces and stops the owner in one turn, so the owner abandons this Show.
    h.output
        .send_modify(|snapshot| snapshot.epoch = snapshot.epoch.next());
    h.runtime.request_stop();
    persisted.release().await?;
    let updates = h.updates.clone();
    h.close().await?;
    assert_eq!(updates.borrow().last_show, Some(saved("newer")));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_new_owner_starts_from_the_last_show_presented() -> anyhow::Result<()> {
    let mut h = Harness::remembering(saved("before a pause"), |_| {})?;
    receive(&h.loads).await?;
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await?;
    assert_eq!(h.snapshot().last_show, Some(saved("before a pause")));
    h.close().await
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
    let recording = h.snapshot();
    assert_eq!((recording.phase, recording.reported), (Phase::Recording, 0));
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.report(Report::ShowCopied)?;
    h.phase(Phase::Copied).await?;
    let copied = h.snapshot();
    assert_eq!(copied.message, SHOW_COPIED);
    assert!(
        copied.show && copied.ending == Ending::Finished,
        "A copy after dictation lacks the label a Show just copied has"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn each_copy_of_the_last_show_presents_anew() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    save_show(&mut h, "look").await?;
    h.phase(Phase::Copied).await?;
    for reported in [1, 2] {
        h.report(Report::ShowCopied)?;
        h.observe(|snapshot| snapshot.reported == reported).await?;
        let snapshot = h.snapshot();
        assert_eq!(
            (snapshot.phase, snapshot.message.as_str()),
            (Phase::Copied, SHOW_COPIED)
        );
    }
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_found_gone_is_a_notice_that_forgets_only_that_show() -> anyhow::Result<()> {
    let mut h = Harness::remembering(saved("first"), |_| {})?;
    h.report(Report::ShowGone(saved("older")))?;
    h.phase(Phase::Notice).await?;
    let snapshot = h.snapshot();
    assert_eq!(snapshot.message, SHOW_GONE);
    assert_eq!(
        snapshot.last_show,
        Some(saved("first")),
        "The Show that replaced the one gone was forgotten"
    );
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.report(Report::ShowGone(saved("first")))?;
    h.observe(|snapshot| snapshot.last_show.is_none()).await?;
    assert_eq!(
        h.snapshot().phase,
        Phase::Recording,
        "The notice interrupted a recording"
    );
    h.close().await
}

#[test]
fn without_an_owner_a_report_shows_while_idle_and_forgets_only_the_show_found_gone() {
    let mut paused = Snapshot {
        message: "Dictation paused".into(),
        last_show: Some(saved("first")),
        narration_lost: Some(NarrationLoss::AudioStopped),
        ..Snapshot::default()
    };
    for reported in [1, 2] {
        assert!(Report::ShowCopied.present(&mut paused));
        assert_eq!(
            (
                paused.reported,
                paused.phase,
                paused.show,
                paused.ending,
                paused.narration_lost
            ),
            (reported, Phase::Copied, true, Ending::Finished, None),
            "Each copy shows anew as a Show just copied"
        );
    }
    assert!(Report::ShowGone(saved("older")).present(&mut paused));
    assert_eq!(
        (paused.phase, paused.last_show.as_ref()),
        (Phase::Notice, Some(&saved("first")))
    );
    for (report, reported) in [
        (Report::Recurring("Disk full".into()), 3),
        (Report::Failed("Disk full".into()), 4),
    ] {
        assert!(report.present(&mut paused));
        assert_eq!(
            (paused.phase, paused.reported),
            (Phase::Error, reported),
            "Only a recurring failure shows again just when it changes"
        );
    }
    assert!(
        !Report::Recurring("Disk full".into()).present(&mut paused),
        "A recurring failure that repeats the one shown reported a change"
    );

    let mut recording = Snapshot {
        phase: Phase::Recording,
        last_show: Some(saved("first")),
        ..Snapshot::default()
    };
    assert!(Report::ShowGone(saved("first")).present(&mut recording));
    assert_eq!(
        (
            recording.phase,
            recording.last_show.as_ref(),
            recording.reported
        ),
        (Phase::Recording, None, 0),
        "A gone Show is forgotten without interrupting the recording"
    );
    assert!(
        !Report::ShowCopied.present(&mut recording),
        "A report interrupted a recording"
    );
}
