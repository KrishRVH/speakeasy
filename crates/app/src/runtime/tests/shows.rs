//! Show scenarios: S, W, or A during a hold, screen lanes, footage, and publication, against a fake
//! screen and gated publication.

use speakeasy_show::Ending;

use super::{
    ports::{PUBLISH_STOPPED, Unchosen},
    *,
};

/// Holds the shortcut and presses S, returning the screen recorder's lane.
async fn start_show(h: &mut Harness) -> anyhow::Result<OpenedShow> {
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    receive(&h.shows).await
}

/// Lets go of the shortcut that started the Show, then presses and releases it to finish.
fn finish_show(h: &Harness) {
    for input in [Input::Release, Input::Press, Input::Release] {
        h.input(input);
    }
}

#[tokio::test(start_paused = true)]
async fn a_show_finishes_on_the_release_after_the_shortcut_is_pressed_again() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    assert!(
        still_pending(h.jobs.recv()).await,
        "Letting go of the shortcut ended a hands-free Show"
    );
    h.input(Input::Press);
    assert!(
        still_pending(h.jobs.recv()).await,
        "Pressing the shortcut finished the Show before its release"
    );
    h.input(Input::Release);
    h.transcribe_next_as("look here").await?;
    let (footage, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("look here".into()));
    assert_eq!(footage.journal.samples().len(), 1);
    h.phase(Phase::Copied).await?;
    let snapshot = h.snapshot();
    assert!(snapshot.message.contains("Show copied"));
    assert_eq!(snapshot.ending, Ending::Finished);
    assert!(h.pasted.try_recv().is_err(), "A Show never pastes");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn w_and_a_record_the_window_or_area_they_name() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    for scope in [Scope::Window, Scope::Area] {
        h.input(Input::Press);
        h.phase(Phase::Recording).await?;
        receive(&h.captures).await?;
        h.input(Input::Show(scope));
        let (_, recorded, _) = receive(&h.shows).await?;
        assert_eq!(recorded, scope);
        h.input(Input::Cancel);
        h.phase(Phase::Cancelled).await?;
        h.input(Input::Release);
    }
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_pin_during_a_show_reaches_its_screen_recorder() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, ..) = start_show(&mut h).await?;
    let taps = double_tap();
    h.input(Input::Pin(taps));
    assert_eq!(receive(&h.pins).await?, (id, taps));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_silent_show_still_publishes_its_screen() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| ports.delayed_finish = true)?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    receive(&h.shows).await?;
    finish_show(&h);
    events.send(CaptureEvent::Finished(id, Ok(None))).await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::default());
    assert!(h.jobs.try_recv().is_err(), "Silence is not transcribed");
    h.phase(Phase::Copied).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_show_frees_the_screen_for_the_next() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    start_show(&mut h).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.input(Input::Release);
    start_show(&mut h).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn escape_alone_leaves_a_hands_free_show_recording_and_publishable() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Escape);
    assert!(
        still_pending(h.phase(Phase::Cancelled)).await,
        "Escape alone cancelled a hands-free Show"
    );
    h.input(Input::Press);
    h.input(Input::Release);
    h.transcribe_next_as("still here").await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("still here".into()));
    h.phase(Phase::Copied).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn escape_cancels_a_show_while_the_shortcut_is_held() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    start_show(&mut h).await?;
    h.input(Input::Escape);
    h.phase(Phase::Cancelled).await?;
    h.input(Input::Release);
    start_show(&mut h).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.input(Input::Escape);
    h.phase(Phase::Cancelled).await?;
    h.input(Input::Release);
    assert!(
        still_pending(h.jobs.recv()).await,
        "Releasing the shortcut finished a cancelled Show"
    );
    assert!(h.published.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_recording_escape_revoked_before_it_became_a_show_stays_dictation_and_cancels()
-> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    tokio::time::advance(Duration::from_secs(1)).await;
    // Delivered back to back, so Escape revokes the recording before the owner sees S.
    for input in [Input::Show(Scope::Screen), Input::Release, Input::Escape] {
        h.input(input);
    }
    h.phase(Phase::Cancelled).await?;
    assert!(
        h.shows.try_recv().is_err(),
        "A revoked recording opened the screen"
    );
    assert!(!h.snapshot().show);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_being_saved_survives_escape_the_tray_and_interruptions() -> anyhow::Result<()> {
    let (publication, publishing) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publication = Some(publication);
    })?;
    start_show(&mut h).await?;
    finish_show(&h);
    let (_, reply) = receive(&h.jobs).await?;
    for input in [Input::Escape, Input::Cancel, Input::Interrupt] {
        h.input(input);
    }
    answer(reply, "this broke")?;
    publishing.reached().await?;
    for input in [
        Input::Press,
        Input::Escape,
        Input::Release,
        Input::Cancel,
        Input::Interrupt,
    ] {
        h.input(input);
    }
    assert!(
        still_pending(h.phase(Phase::Cancelled)).await,
        "A Show being saved was discarded"
    );
    publishing.release().await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("this broke".into()));
    h.phase(Phase::Copied).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_that_cannot_be_published_explains_why() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publish_error = Some("Disk full");
    })?;
    start_show(&mut h).await?;
    finish_show(&h);
    h.transcribe_next_as("look").await?;
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Disk full"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_publication_that_panics_reports_the_fixed_show_failure() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| ports.publish_panics = true)?;
    start_show(&mut h).await?;
    finish_show(&h);
    h.transcribe_next_as("look").await?;
    h.phase(Phase::Error).await?;
    assert_eq!(h.snapshot().message, PUBLISH_STOPPED);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn shutdown_awaits_a_retiring_screen() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.screen_retirement = Some(retirement);
    })?;
    start_show(&mut h).await?;
    h.runtime.request_stop();
    retiring.reached().await?;
    assert!(still_pending(h.exited()).await);
    retiring.release().await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn shutdown_revokes_and_owns_pending_publication_cleanup() -> anyhow::Result<()> {
    let (publication, publishing) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publication = Some(publication);
    })?;
    start_show(&mut h).await?;
    finish_show(&h);
    h.transcribe_next_as("fixture").await?;
    publishing.reached().await?;
    h.runtime.request_stop();
    assert!(still_pending(h.exited()).await);
    publishing.release().await?;
    timeout(PATIENCE, h.exited()).await?;
    assert!(
        h.published.try_recv().is_err(),
        "A revoked Show was published"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_screen_that_cannot_be_recorded_explains_why() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.screen_error = Some("Screen recording denied");
    })?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    h.input(Input::Show(Scope::Screen));
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Screen recording denied"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn s_with_no_recording_records_no_screen_then_or_later() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Show(Scope::Screen));
    h.start().await?;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(h.shows.try_recv().is_err());
    assert!(!h.snapshot().show);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn s_after_space_locked_the_hold_still_starts_a_show() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Lock);
    h.observe(|snapshot| snapshot.hands_free).await?;
    h.input(Input::Show(Scope::Screen));
    h.observe(|snapshot| snapshot.show).await?;
    receive(&h.shows).await?;
    finish_show(&h);
    h.transcribe_next_as("locked").await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("locked".into()));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn s_while_the_last_microphone_retires_still_starts_a_show() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(2);
    let (screen_retirement, screen_retiring) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
        ports.screen_retirement = Some(screen_retirement);
    })?;
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    retiring.reached().await?;
    h.input(Input::Press);
    h.phase(Phase::Starting).await?;
    h.input(Input::Show(Scope::Screen));
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    receive(&h.shows).await?;
    assert!(
        still_pending(screen_retiring.reached()).await,
        "The screen stopped before the Show's narration began"
    );
    retiring.release().await?;
    receive(&h.captures).await?;
    finish_show(&h);
    h.transcribe_next_as("queued").await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("queued".into()));
    for gate in [&retiring, &screen_retiring] {
        gate.reached().await?;
        gate.release().await?;
    }
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_microphone_failure_ends_the_show_and_frees_the_screen() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    receive(&h.shows).await?;
    events
        .send(CaptureEvent::Finished(
            id,
            Err(anyhow!("Microphone disconnected")),
        ))
        .await?;
    h.phase(Phase::Error).await?;
    h.input(Input::Release);
    start_show(&mut h).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_screen_that_stops_recording_explains_why_and_frees_the_microphone() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    let (id, _, events) = start_show(&mut h).await?;
    events
        .send(ShowEvent::Finished(
            id,
            Err(anyhow!("Screen recording is off")),
        ))
        .await?;
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Screen recording is off"));
    h.input(Input::Release);
    h.input(Input::Press);
    receive(&h.captures).await?;
    h.close().await
}

/// Footage whose screen stopped recording before the Show was finished.
fn stopped_footage() -> Box<Footage> {
    let mut footage = footage();
    footage.recording.ending = Ending::ScreenStopped;
    footage
}

#[tokio::test(start_paused = true)]
async fn a_screen_that_stops_partway_finishes_the_show_and_says_why() -> anyhow::Result<()> {
    for (ending, message) in [
        (
            Ending::ScreenStopped,
            "Show copied, but the screen stopped recording early.",
        ),
        (
            Ending::WindowClosed,
            "Show copied, but the recorded window closed.",
        ),
        (
            Ending::WindowOffScreen,
            "Show copied, but the recorded window went off screen.",
        ),
    ] {
        let mut h = Harness::new()?;
        let (id, _, events) = start_show(&mut h).await?;
        let mut stopped = footage();
        stopped.recording.ending = ending;
        events.send(ShowEvent::Finished(id, Ok(stopped))).await?;
        h.transcribe_next_as("before it stopped").await?;
        let (published, narration) = receive(&h.published).await?;
        assert_eq!(narration, Narration::Text("before it stopped".into()));
        assert_eq!(published.recording.ending, ending);
        h.phase(Phase::Copied).await?;
        let snapshot = h.snapshot();
        assert_eq!(snapshot.ending, ending, "The pill presents the notice");
        assert_eq!(snapshot.message, message);
        h.close().await?;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn w_with_no_window_to_record_cancels_the_show_and_says_why() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show(Scope::Window));
    let (id, _, events) = receive(&h.shows).await?;
    h.take_sounds();
    events
        .send(ShowEvent::Unchosen(id, Unchosen::Window))
        .await?;
    h.phase(Phase::Unchosen).await?;
    assert_eq!(
        h.snapshot().message,
        "No window to record under the pointer, so the Show was cancelled."
    );
    assert_eq!(
        h.take_sounds(),
        [Sound::Played(Cue::Cancel), Sound::Held(false)]
    );
    h.input(Input::Release);
    assert!(
        still_pending(h.jobs.recv()).await,
        "The cancelled Show's narration was transcribed"
    );
    assert!(
        h.published.try_recv().is_err(),
        "A cancelled Show was saved"
    );
    assert!(h.pasted.try_recv().is_err(), "Its narration was pasted");
    h.input(Input::Press);
    receive(&h.captures).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn an_area_show_finished_before_its_area_was_chosen_saves_and_pastes_nothing()
-> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.delayed_finish = true;
        ports.screen_unchosen = Some(Unchosen::Area);
    })?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, audio) = receive(&h.captures).await?;
    h.input(Input::Show(Scope::Area));
    receive(&h.shows).await?;
    finish_show(&h);
    h.phase(Phase::Unchosen).await?;
    assert_eq!(
        h.snapshot().message,
        "No area was chosen, so the Show was cancelled."
    );
    audio
        .send(CaptureEvent::Finished(id, Ok(Some(sealed()))))
        .await?;
    assert!(
        still_pending(h.jobs.recv()).await,
        "The cancelled Show's narration was transcribed"
    );
    assert!(
        h.published.try_recv().is_err(),
        "A cancelled Show was saved"
    );
    assert!(h.pasted.try_recv().is_err(), "Its narration was pasted");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn new_settings_discard_a_show_whose_screen_stopped_while_it_is_saved() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    let (id, _, events) = start_show(&mut h).await?;
    events
        .send(ShowEvent::Finished(id, Ok(stopped_footage())))
        .await?;
    let (_, mut reply) = receive(&h.jobs).await?;
    let config = Config {
        remove_fillers: !h.runtime.configuration.borrow().remove_fillers,
        ..h.runtime.configuration.borrow().clone()
    };
    h.runtime.configure(ValidConfig::assume(config));
    h.phase(Phase::Cancelled).await?;
    timeout(PATIENCE, reply.closed()).await?;
    assert!(
        answer(reply, "too late").is_err(),
        "The discarded Show's transcription stayed alive"
    );
    assert!(
        still_pending(h.published.recv()).await,
        "A discarded Show was published"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn footage_from_an_earlier_show_never_reaches_a_later_one() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (earlier, _, stale) = start_show(&mut h).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.input(Input::Release);
    start_show(&mut h).await?;
    let mut late = footage();
    late.recording.duration = Moment::from_millis(9_999);
    stale.send(ShowEvent::Finished(earlier, Ok(late))).await?;
    assert!(
        still_pending(h.published.recv()).await,
        "A Show published while it was still recording"
    );
    finish_show(&h);
    h.transcribe_next_as("this one").await?;
    let (published, _) = receive(&h.published).await?;
    assert_eq!(published.recording, footage().recording);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn an_earlier_show_with_nothing_to_record_never_cancels_a_later_one() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show(Scope::Window));
    let (earlier, _, stale) = receive(&h.shows).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.input(Input::Release);
    start_show(&mut h).await?;
    h.take_sounds();
    stale
        .send(ShowEvent::Unchosen(earlier, Unchosen::Window))
        .await?;
    assert!(
        still_pending(h.observe(|snapshot| snapshot.phase != Phase::Recording)).await,
        "The later Show stopped recording"
    );
    assert!(
        !h.take_sounds().contains(&Sound::Played(Cue::Cancel)),
        "The later Show sounded cancelled"
    );
    finish_show(&h);
    h.transcribe_next_as("this one").await?;
    receive(&h.published).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_queued_behind_a_retiring_screen_still_records_it() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(2);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.screen_retirement = Some(retirement);
    })?;
    start_show(&mut h).await?;
    finish_show(&h);
    h.transcribe_next_as("first").await?;
    receive(&h.published).await?;
    retiring.reached().await?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    h.observe(|snapshot| snapshot.show && snapshot.hands_free)
        .await?;
    finish_show(&h);
    h.transcribe_next_as("second").await?;
    assert!(
        still_pending(h.shows.recv()).await,
        "The second Show's screen opened before the first let go"
    );
    retiring.release().await?;
    receive(&h.shows).await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("second".into()));
    retiring.reached().await?;
    retiring.release().await?;
    h.close().await
}
