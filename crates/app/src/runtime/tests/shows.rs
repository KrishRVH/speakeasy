use speakeasy_show::Ending;

use super::{ports::Unchosen, *};
use crate::audio::SilentMicrophone;

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
async fn a_tray_show_records_its_scope_hands_free_until_toggled() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    for scope in [Scope::Screen, Scope::Area] {
        h.input(Input::StartShow(scope));
        let (_, recorded, _) = receive(&h.shows).await?;
        assert_eq!(recorded, scope);
        let (id, _) = receive(&h.captures).await?;
        h.observe(|snapshot| {
            snapshot.phase == Phase::Recording && snapshot.show && snapshot.hands_free
        })
        .await?;
        assert_eq!(
            h.take_sounds(),
            [
                Sound::Held(true),
                Sound::Expected(id),
                Sound::Played(Cue::Show)
            ],
            "{scope:?}"
        );
        h.input(Input::Toggle);
        h.transcribe_next_as("from the tray").await?;
        let (_, narration) = receive(&h.published).await?;
        assert_eq!(narration, Narration::Text("from the tray".into()));
        h.phase(Phase::Copied).await?;
        assert_eq!(
            h.take_sounds(),
            [
                Sound::Played(Cue::Stop),
                Sound::Played(Cue::Copied),
                Sound::Held(false)
            ],
            "{scope:?}"
        );
    }
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_tray_show_leaves_a_session_under_way_alone() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    h.input(Input::StartShow(Scope::Screen));
    assert!(
        still_pending(h.observe(|snapshot| snapshot.show || snapshot.phase != Phase::Recording))
            .await,
        "A tray Show turned or ended the dictation under way"
    );
    assert!(h.shows.try_recv().is_err(), "A tray Show opened the screen");
    h.finish_as("still dictation").await?;
    assert_eq!(receive(&h.pasted).await?, "still dictation");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_that_cannot_be_published_explains_why() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.publish_outcome = PublishOutcome::Fails("Disk full");
    })?;
    start_show(&mut h).await?;
    finish_show(&h);
    h.transcribe_next_as("look").await?;
    h.phase(Phase::Error).await?;
    assert!(h.snapshot().message.contains("Disk full"));
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

const COPIED_WITHOUT_NARRATION: &str = "Show copied without narration.";

const DISCONNECTED: &str = "The microphone was disconnected. Reconnect it.";

fn disconnected_before_speech(id: SessionId) -> CaptureEvent {
    CaptureEvent::Finished(id, Err(anyhow!(DISCONNECTED)))
}

#[tokio::test(start_paused = true)]
async fn a_show_whose_microphone_is_lost_keeps_the_narration_that_arrived() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, events) = start_show_listening(&mut h).await?;
    receive(&h.shows).await?;
    events.send(disconnected(id)).await?;
    h.transcribe_next_as("look at this").await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("look at this".into()));
    h.phase(Phase::Copied).await?;
    let snapshot = h.snapshot();
    assert_eq!(
        snapshot.message,
        "Show copied with partial narration. The microphone was disconnected."
    );
    assert_eq!(snapshot.narration_lost, Some(NarrationLoss::Incomplete));
    assert!(
        h.take_sounds().contains(&Sound::Played(Cue::Stop)),
        "The Show ended where its microphone stopped"
    );
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_whose_audio_stops_ends_there_and_is_copied_with_the_remedy() -> anyhow::Result<()> {
    let (results, loading) = async_channel::bounded(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.load_results = Some(loading);
    })?;
    receive(&h.loads).await?;
    let (id, events) = start_show_listening(&mut h).await?;
    receive(&h.shows).await?;
    events.send(disconnected_before_speech(id)).await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::default());
    assert!(
        h.jobs.try_recv().is_err(),
        "No audio was kept to transcribe"
    );
    h.phase(Phase::Copied).await?;
    let snapshot = h.snapshot();
    let copied = format!("{COPIED_WITHOUT_NARRATION} {DISCONNECTED}");
    assert_eq!(snapshot.message, copied);
    assert_eq!(snapshot.narration_lost, Some(NarrationLoss::AudioStopped));
    assert!(
        h.take_sounds().contains(&Sound::Played(Cue::Stop)),
        "The Show ended where its audio stopped"
    );
    results.send(Ok(())).await?;
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await?;
    assert_eq!(
        h.snapshot().message,
        copied,
        "A model that loads fixes nothing the microphone needs"
    );
    h.input(Input::Release);
    start_show(&mut h).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_whose_microphone_sent_only_silence_is_copied_with_the_remedy() -> anyhow::Result<()>
{
    let mut h = Harness::with_ports(Config::default(), |ports| ports.delayed_finish = true)?;
    let (id, microphone) = start_show_listening(&mut h).await?;
    receive(&h.shows).await?;
    finish_show(&h);
    h.phase(Phase::Stopping).await?;
    microphone
        .send(CaptureEvent::Finished(id, Err(SilentMicrophone.into())))
        .await?;
    h.phase(Phase::Copied).await?;
    let snapshot = h.snapshot();
    assert_eq!(
        (snapshot.ending, snapshot.narration_lost),
        (Ending::Finished, Some(NarrationLoss::AudioSilent))
    );
    assert_eq!(
        snapshot.message,
        format!("{COPIED_WITHOUT_NARRATION} {SilentMicrophone}")
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_whose_speech_fails_its_retry_is_copied_without_narration() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    start_show(&mut h).await?;
    finish_show(&h);
    let (_, reply) = receive(&h.jobs).await?;
    assert!(reply.send(Err(anyhow!("Engine exited"))).is_ok());
    receive(&h.loads).await?;
    let (_, retry) = receive(&h.jobs).await?;
    assert!(retry.send(Err(anyhow!("Engine exited again"))).is_ok());
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::default());
    h.phase(Phase::Copied).await?;
    let snapshot = h.snapshot();
    assert_eq!(
        snapshot.message,
        format!("{COPIED_WITHOUT_NARRATION} Engine exited again")
    );
    assert_eq!(snapshot.narration_lost, Some(NarrationLoss::NotTranscribed));
    receive(&h.loads).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_model_that_fails_to_load_fails_dictation_but_copies_a_show_without_narration()
-> anyhow::Result<()> {
    let (results, loading) = async_channel::bounded(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.load_results = Some(loading);
    })?;
    receive(&h.loads).await?;
    h.start().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Processing).await?;
    results.send(Err(anyhow!("Model failed to load"))).await?;
    h.phase(Phase::Error).await?;
    assert_eq!(h.snapshot().message, "Model failed to load");

    start_show(&mut h).await?;
    finish_show(&h);
    receive(&h.loads).await?;
    h.phase(Phase::Processing).await?;
    results.send(Err(anyhow!("Model failed to load"))).await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::default());
    h.phase(Phase::Copied).await?;
    let snapshot = h.snapshot();
    assert_eq!(
        snapshot.message,
        format!("{COPIED_WITHOUT_NARRATION} Model failed to load")
    );
    assert_eq!(snapshot.narration_lost, Some(NarrationLoss::NotTranscribed));
    assert_eq!(
        snapshot.model,
        ModelState::Unavailable,
        "Settings and the tray still say the model needs attention"
    );
    assert!(h.jobs.try_recv().is_err(), "Nothing was transcribed");
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
        ports.screen_report = ScreenReport::Unchosen(Unchosen::Area);
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
    change_settings(&mut h);
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
async fn a_show_keeps_the_display_on_while_it_records() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    start_show(&mut h).await?;
    let held: Vec<_> = std::iter::from_fn(|| h.awake.try_recv().ok()).collect();
    assert_eq!(held.last(), Some(&Some(Keep::Display)), "{held:?}");
    h.close().await
}
