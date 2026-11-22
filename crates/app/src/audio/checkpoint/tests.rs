use std::{iter, time::SystemTime};

use anyhow::Context as _;
use speakeasy_core::machine::Cue;

use super::*;
use crate::{
    audio::{fixture, pcm::SAMPLE_BYTES},
    show::{Keeper, Kept},
};

const RATE: u32 = 48_000;
const SECOND: usize = RATE as usize * SAMPLE_BYTES;

async fn promote(before_cue: Vec<u8>) -> anyhow::Result<(Option<Sealed>, Option<Sealed>)> {
    let root = tempfile::tempdir()?;
    let parent = root.path().to_owned();
    let (send, incoming) = async_channel::bounded(1);
    let (cues, heard) = async_channel::unbounded();
    let mut keeping = Keeping::new(&incoming);
    let mut speech = Audibility::new(RATE);
    let mut pcm = before_cue;
    keeping.classify(&mut speech, &pcm, &heard);
    cues.try_send(())?;
    pcm.extend(fixture::loud_cue(Cue::Show));
    keeping.classify(&mut speech, &pcm, &heard);
    let Kept {
        keeper,
        frames,
        audio,
    } = Keeper::start(move || Ok(parent), SystemTime::now())?;
    drop(frames);
    send.try_send(Keep {
        log: audio,
        start: Duration::from_millis(125),
        language: "fr".into(),
    })
    .map_err(|_refused| anyhow::anyhow!("Synthetic checkpoint was refused"))?;
    assert!(keeping.offer(&pcm, RATE));
    pcm.extend(iter::repeat_n(0, 2 * SECOND));
    keeping.classify(&mut speech, &pcm, &heard);
    let (events, warnings) = async_channel::unbounded();
    keeping
        .close(&pcm, RATE, AudioEnding::Complete, SessionId::FIRST, &events)
        .context("Kept audio has no tail")?
        .flush();
    assert!(warnings.is_empty());
    assert!(incoming.is_closed());
    let checkpoint = keeper
        .close()
        .await?
        .context("Fixture checkpoint is missing")?;
    let audio = checkpoint.audio()?.unwrap();
    assert!(audio.pcm == pcm, "Checkpoint changed synthetic PCM");
    assert!(audio.complete);
    assert_eq!(audio.start, Duration::from_millis(125));
    assert_eq!(&*audio.language, "fr");
    let live = Pcm16::from_kept(&pcm).seal(speech, RATE)?;
    let recovered = seal_kept(&audio)?;
    checkpoint.discard()?;
    Ok((live, recovered))
}

#[tokio::test]
async fn a_promoted_cue_alone_replays_as_no_speech() -> anyhow::Result<()> {
    let (live, recovered) = promote(Vec::new()).await?;
    assert!(
        live.is_none(),
        "The live gate counted its own cue as speech"
    );
    assert!(recovered.is_none(), "Replay counted the kept cue as speech");
    Ok(())
}

#[tokio::test]
async fn promoted_speech_replays_to_the_same_sealed_audio() -> anyhow::Result<()> {
    let speech = fixture::square_wave(4_000)
        .flat_map(i16::to_le_bytes)
        .take(SECOND / 2)
        .chain(iter::repeat_n(0, SECOND))
        .collect();
    let (Some(live), Some(recovered)) = promote(speech).await? else {
        anyhow::bail!("Speech before the cue was not sealed");
    };
    assert!(recovered.wav == live.wav, "Replay changed sealed audio");
    assert_eq!(recovered.trimmed, live.trimmed);
    Ok(())
}
