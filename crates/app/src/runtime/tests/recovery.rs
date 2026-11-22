use anyhow::Context as _;
use speakeasy_show::Take;

use super::*;
use crate::audio::fixture::square_wave;

async fn interrupted() -> anyhow::Result<(tempfile::TempDir, Interrupted)> {
    interrupted_with_audio(false).await
}

async fn interrupted_with_audio(
    keep_audio: bool,
) -> anyhow::Result<(tempfile::TempDir, Interrupted)> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let parent = root.path().to_owned();
    let show::Kept {
        keeper,
        mut frames,
        mut audio,
    } = show::Keeper::start(move || Ok(parent), now)?;
    assert!(frames.begin(
        1_000,
        Size {
            width: 80.0,
            height: 60.0
        },
        speakeasy_show::Framing::Automatic
    ));
    if keep_audio {
        assert!(audio.begin(8_000, Duration::ZERO, "fr".into()));
        audio.finish(
            &square_wave(3000)
                .take(8_000)
                .flat_map(i16::to_le_bytes)
                .collect::<Vec<_>>(),
            &[],
            show::checkpoint::AudioEnding::Complete,
        );
    } else {
        drop(audio);
    }
    let mut take = Take::new(
        1_000,
        Size {
            width: 80.0,
            height: 60.0,
        },
        speakeasy_show::Framing::Automatic,
    );
    let event = take.arrive(Moment::from_millis(100), || {
        Some(Image::from_bgra(2, 2, [3, 2, 1, 255].repeat(4)).unwrap())
    })?;
    assert!(frames.send(event));
    drop(frames);
    let checkpoint = keeper
        .close()
        .await?
        .context("Fixture checkpoint is missing")?;
    let interrupted = checkpoint.interrupted();
    Ok((root, interrupted))
}

#[tokio::test(start_paused = true)]
async fn recovery_waits_for_dictation_then_saves_without_microphone_speech_or_copy()
-> anyhow::Result<()> {
    let (_root, interrupted) = interrupted().await?;
    let (recovery, recovering) = gate(1);
    let mut h = Harness::with_ports(Config::default(), |ports| {
        ports.show_recovery = Some(recovery);
    })?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    let receipt = h
        .runtime
        .recover(interrupted.clone())
        .map_err(anyhow::Error::msg)?;
    assert!(still_pending(recovering.reached()).await);
    assert!(h.runtime.recover(interrupted).is_err());
    h.input(Input::Cancel);
    h.input(Input::Release);
    let authority = recovering.reached().await?;
    h.phase(Phase::Processing).await?;
    for input in [Input::Press, Input::Escape, Input::Cancel, Input::Release] {
        h.input(input);
    }
    assert!(still_pending(h.phase(Phase::Cancelled)).await);
    assert!(authority.active());
    recovering.release().await?;
    let saved = timeout(PATIENCE, receipt)
        .await??
        .map_err(anyhow::Error::msg)?;
    h.phase(Phase::Notice).await?;
    assert_eq!(h.snapshot().last_show, Some(saved));
    assert!(h.captures.try_recv().is_err());
    assert!(h.jobs.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    assert!(h.published.try_recv().is_err());
    h.close().await
}

mod audio;
