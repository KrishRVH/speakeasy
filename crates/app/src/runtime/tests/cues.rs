use super::*;

async fn record_a_show(h: &mut Harness) -> anyhow::Result<SessionId> {
    let (id, ..) = start_show(h).await?;
    finish_show(h);
    h.transcribe_next_as("look here").await?;
    h.phase(Phase::Copied).await?;
    Ok(id)
}

#[tokio::test(start_paused = true)]
async fn a_recording_sounds_its_start_once_listening_and_its_stop() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, _) = h.start().await?;
    assert_eq!(
        h.take_sounds(),
        [
            Sound::Held(true),
            Sound::Expected(id),
            Sound::Played(Cue::Start)
        ]
    );
    h.finish_as("fixture").await?;
    h.phase(Phase::Done).await?;
    assert_eq!(
        h.take_sounds(),
        [Sound::Played(Cue::Stop), Sound::Held(false)]
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn another_shortcut_taking_the_keys_ends_the_recording_silently_and_lets_the_output_close()
-> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    h.take_sounds();
    h.input(Input::Interrupt);
    h.input(Input::Release);
    h.phase(Phase::Cancelled).await?;
    assert_eq!(h.take_sounds(), [Sound::Held(false)]);
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn saving_settings_during_a_recording_sounds_its_cancel() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    h.start().await?;
    h.take_sounds();
    change_settings(&mut h);
    h.phase(Phase::Cancelled).await?;
    assert_eq!(
        h.take_sounds(),
        [Sound::Played(Cue::Cancel), Sound::Held(false)]
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_show_tells_its_microphone_before_its_cue_and_sounds_once_copied() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let id = record_a_show(&mut h).await?;
    assert_eq!(
        h.take_sounds(),
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
    h.take_sounds();
    h.finish_as("fixture").await?;
    h.phase(Phase::Copied).await?;
    assert_eq!(
        h.take_sounds(),
        [
            Sound::Played(Cue::Stop),
            Sound::Played(Cue::Copied),
            Sound::Held(false)
        ]
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn copy_last_show_sounds_its_chord_only_for_a_copy_while_idle() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, events) = h.start().await?;
    h.take_sounds();
    h.report(Report::ShowCopied)?;
    events.send(CaptureEvent::Level(id, 0.5)).await?;
    h.observe_level(0.5).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    assert_eq!(
        h.take_sounds(),
        [Sound::Played(Cue::Cancel), Sound::Held(false)],
        "A copy sounded during a recording"
    );

    for report in [
        Report::ShowGone(SavedShow::assume("shows/gone".to_owned())),
        Report::Failed("Public fixture failure".into()),
        Report::ShowCopied,
    ] {
        h.report(report)?;
    }
    h.observe(|snapshot| snapshot.reported == 3).await?;
    assert_eq!(h.take_sounds(), [Sound::Played(Cue::Copied)]);
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
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.report(Report::ShowCopied)?;
    h.phase(Phase::Copied).await?;
    assert_eq!(h.take_sounds(), []);
    h.close().await
}
