//! Late promotion and replay use synthetic PCM and real private storage, never a device.

use std::time::SystemTime;

use super::*;
use crate::{
    show::{Keeper, Kept},
    sounds::Cue,
};

#[tokio::test]
async fn promotion_keeps_buffered_audio_and_cues_and_replays_the_live_speech_gate()
-> anyhow::Result<()> {
    for spoken in [false, true] {
        let root = tempfile::tempdir()?;
        let parent = root.path().to_owned();
        let (send, incoming) = async_channel::bounded(1);
        let (cues, heard) = async_channel::unbounded();
        let mut keeping = Keeping::new(&incoming);
        let mut speech = Audibility::new(48_000);
        let mut pcm = Vec::new();
        if spoken {
            // This speech predates the cue. Replaying the cue over all prior audio would wrongly
            // erase the high-frequency voice in this synthetic test.
            pcm.extend(
                (0..24_000)
                    .flat_map(|i| if i % 12 < 6 { 4_000_i16 } else { -4_000_i16 }.to_le_bytes()),
            );
            keeping.classify(&mut speech, &pcm, &heard);
            pcm.extend(std::iter::repeat_n(0, 96_000));
            keeping.classify(&mut speech, &pcm, &heard);
        }
        cues.try_send(())?;
        pcm.extend(
            Cue::Show
                .pcm()
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|pair| i16::from_le_bytes(*pair).saturating_mul(3).to_le_bytes()),
        );
        keeping.classify(&mut speech, &pcm, &heard);
        assert!(
            !root.path().join(".checkpoints").exists(),
            "Dictation wrote a checkpoint"
        );
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
        })?;
        assert!(keeping.offer(&pcm, 48_000));
        pcm.extend(std::iter::repeat_n(0, 192_000));
        keeping.classify(&mut speech, &pcm, &heard);
        assert!(keeping.finish(&pcm, 48_000));
        assert!(incoming.is_closed());
        let checkpoint = keeper.close().await?;
        let audio = checkpoint.audio()?.unwrap();
        assert!(*audio.pcm == pcm, "Checkpoint changed synthetic PCM");
        assert!(audio.complete);
        let live = crate::audio::pcm::Pcm16::from_kept(zeroize::Zeroizing::new(pcm))
            .seal(speech, 48_000)?;
        let (recovered, start, language) = seal_kept(audio)?;
        assert_eq!(start, Duration::from_millis(125));
        assert_eq!(&*language, "fr");
        assert_eq!(live.is_some(), spoken);
        assert_eq!(recovered.is_some(), live.is_some());
        if let (Some(recovered), Some(live)) = (recovered, live) {
            assert!(recovered.wav == live.wav, "Replay changed sealed audio");
            assert_eq!(recovered.trimmed, live.trimmed);
        }
        checkpoint.discard()?;
    }
    Ok(())
}
