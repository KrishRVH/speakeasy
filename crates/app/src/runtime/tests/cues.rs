//! Sound cues against the fake ports: which moments sound, that a microphone expects each cue it
//! could pick up before that cue plays, that the cue output is held open exactly through each
//! session, and that turning sounds off silences all three.

use super::*;

/// Holds the shortcut, presses S, then finishes the Show and lets it publish, returning the
/// microphone's session.
async fn record_a_show(h: &mut Harness) -> anyhow::Result<SessionId> {
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, _) = receive(&h.captures).await?;
    h.input(Input::Show(Scope::Screen));
    receive(&h.shows).await?;
    h.input(Input::Release);
    h.input(Input::Press);
    h.transcribe_next_as("look here").await?;
    h.phase(Phase::Copied).await?;
    Ok(id)
}

#[tokio::test(start_paused = true)]
async fn a_recording_sounds_its_start_once_listening_and_its_stop() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, _) = h.start().await?;
    assert_eq!(
        h.sounds(),
        [
            Sound::Held(true),
            Sound::Expected(id),
            Sound::Played(Cue::Start)
        ]
    );
    h.finish_as("fixture").await?;
    h.phase(Phase::Done).await?;
    assert_eq!(h.sounds(), [Sound::Played(Cue::Stop), Sound::Held(false)]);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_recording_finished_before_its_microphone_listens_sounds_only_its_stop()
-> anyhow::Result<()> {
    let mut h = Harness::new()?;
    // The second toggle outranks the microphone's readiness, which then finds the session stopping.
    h.input(Input::Toggle);
    h.input(Input::Toggle);
    h.transcribe_next_as("fixture").await?;
    h.phase(Phase::Done).await?;
    assert_eq!(
        h.sounds(),
        [
            Sound::Held(true),
            Sound::Played(Cue::Stop),
            Sound::Held(false)
        ]
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn the_five_minute_limit_and_a_cancel_sound_as_the_recording_ends() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    h.sounds();
    tokio::time::advance(Duration::from_secs(300)).await;
    h.transcribe_next_as("fixture").await?;
    h.phase(Phase::Done).await?;
    assert_eq!(h.sounds(), [Sound::Played(Cue::Stop), Sound::Held(false)]);

    h.start().await?;
    h.sounds();
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    assert_eq!(h.sounds(), [Sound::Played(Cue::Cancel), Sound::Held(false)]);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_capture_that_ends_before_its_gesture_still_sounds_the_stop() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, events) = h.start().await?;
    h.sounds();
    events.try_send(CaptureEvent::Finished(id, Ok(Some(sealed()))))?;
    h.transcribe_next_as("fixture").await?;
    h.phase(Phase::Done).await?;
    assert_eq!(h.sounds(), [Sound::Played(Cue::Stop), Sound::Held(false)]);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn another_shortcut_taking_the_keys_ends_the_recording_silently_and_lets_the_output_close()
-> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    h.sounds();
    h.input(Input::Interrupt);
    h.input(Input::Release);
    h.phase(Phase::Cancelled).await?;
    assert_eq!(h.sounds(), [Sound::Held(false)]);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn turning_sounds_off_during_a_recording_lets_the_output_close() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    h.sounds();
    let config = Config {
        play_sounds: false,
        ..h.runtime.configuration.borrow().clone()
    };
    h.runtime.configure(ValidConfig::assume(config));
    h.phase(Phase::Idle).await?;
    assert_eq!(h.sounds(), [Sound::Held(false)]);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_microphone_still_opening_sounds_its_cue_once_it_listens() -> anyhow::Result<()> {
    let (retirement, retiring) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.retirement = Some(retirement);
    })?;
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    retiring.reached().await?;
    h.sounds();
    // The next hold waits for that microphone with the output already open; S meanwhile makes it
    // a Show, still unheard.
    h.input(Input::Press);
    h.phase(Phase::Starting).await?;
    h.input(Input::Show(Scope::Screen));
    receive(&h.shows).await?;
    assert_eq!(h.sounds(), [Sound::Held(true)]);
    retiring.release().await?;
    let (id, _) = receive(&h.captures).await?;
    h.phase(Phase::Recording).await?;
    assert_eq!(h.sounds(), [Sound::Expected(id), Sound::Played(Cue::Show)]);
    h.input(Input::Cancel);
    retiring.reached().await?;
    retiring.release().await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_tells_its_microphone_before_its_cue_and_sounds_once_copied() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let id = record_a_show(&mut h).await?;
    assert_eq!(
        h.sounds(),
        [
            Sound::Held(true),
            Sound::Expected(id),
            Sound::Played(Cue::Start),
            Sound::Expected(id),
            Sound::Played(Cue::Show),
            Sound::Played(Cue::Stop),
            Sound::Played(Cue::Copied),
            Sound::Held(false),
        ]
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn dictation_left_on_the_clipboard_sounds_as_copied() -> anyhow::Result<()> {
    let (results, queued) = async_channel::bounded(1);
    results.try_send(Inserted::Copied("Text copied."))?;
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.insert_results = Some(queued);
    })?;
    h.start().await?;
    h.sounds();
    h.finish_as("fixture").await?;
    h.phase(Phase::Copied).await?;
    assert_eq!(
        h.sounds(),
        [
            Sound::Played(Cue::Stop),
            Sound::Played(Cue::Copied),
            Sound::Held(false)
        ]
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn with_sounds_off_nothing_plays_or_holds_the_output_and_no_microphone_expects_a_cue()
-> anyhow::Result<()> {
    let config = Config {
        play_sounds: false,
        ..Config::default()
    };
    let mut h = Harness::with_ports(config, |_| {})?;
    record_a_show(&mut h).await?;
    h.input(Input::Release);
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    assert_eq!(h.sounds(), []);
    h.close().await
}
