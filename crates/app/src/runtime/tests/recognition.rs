use super::*;
use crate::{
    audio::{silent_wav, wav_sample_rate},
    config::Engine,
    runtime::ports::Partition,
    transcript::vocabulary::Correction,
};

fn config() -> Config {
    Config {
        engine: Engine::Nemo,
        language: "en".into(),
        use_gpu: false,
        ..Config::default()
    }
}

#[tokio::test(start_paused = true)]
async fn segmentation_admits_only_nemo_dictation_regardless_of_language() -> anyhow::Result<()> {
    for (engine, language, show, expected) in [
        (Engine::Nemo, "en", false, true),
        (Engine::Nemo, "auto", false, true),
        (Engine::Whisper, "en", false, false),
        (Engine::Whisper, "auto", false, false),
        (Engine::Nemo, "auto", true, false),
    ] {
        let (segmentation, admitted) = async_channel::bounded(1);
        let mut h = Harness::with_ports(
            Config {
                engine,
                language: language.into(),
                ..config()
            },
            |ports| ports.segmentation = Some(segmentation),
        )?;
        h.input(if show {
            Input::StartShow(Scope::Screen)
        } else {
            Input::Toggle
        });
        h.phase(Phase::Recording).await?;
        assert_eq!(receive(&admitted).await?, expected);
        h.close().await?;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_nonempty_tail_keeps_every_piece_and_cleans_across_the_final_boundary()
-> anyhow::Result<()> {
    let mut config = config();
    config.vocabulary = vec![Correction {
        heard: "cube control".into(),
        written: "kubectl".into(),
    }];
    let (recorded, requests) = async_channel::bounded(3);
    let mut h = Harness::with_ports(config, |ports| {
        ports.delayed_finish = true;
        ports.recorded_requests = Some(recorded);
    })?;
    let (id, events) = h.start().await?;
    for (number, text) in [(0, "Ready,"), (1, "um. cube")] {
        events
            .send(CaptureEvent::Piece(id, number, piece()))
            .await?;
        h.transcribe_next_as(text).await?;
        assert_eq!(receive(&requests).await?, *sealed().wav);
    }
    assert!(still_pending(h.pasted.recv()).await);
    let mut audio = silent_wav(16_000, Duration::from_secs(2))?;
    audio[44..32_044].fill(0x55);
    audio[32_044..].fill(0x2A);
    events
        .send(CaptureEvent::Finished(
            id,
            Ok(Some(Sealed {
                wav: audio.clone(),
                trimmed: Duration::ZERO,
                audible: SPOKEN,
                partition: Some(Partition {
                    pieces: 2,
                    tail: 32_000..64_000,
                }),
            })),
        ))
        .await?;
    let (_, reply) = receive(&h.jobs).await?;
    let tail = receive(&requests).await?;
    assert_eq!(&tail[44..], &audio[32_044..]);
    assert_eq!(wav_sample_rate(&tail)?, 16_000);
    assert!(still_pending(h.pasted.recv()).await);
    answer(reply, "control")?;
    assert_eq!(receive(&h.pasted).await?, "Ready. kubectl");
    h.phase(Phase::Done).await?;
    assert!(h.pasted.try_recv().is_err());
    assert!(h.jobs.try_recv().is_err());
    h.close().await
}

const BRIEF: Duration = Duration::from_millis(900);

fn heard_for(audible: Duration) -> Sealed {
    Sealed {
        wav: vec![1, 2],
        trimmed: Duration::ZERO,
        audible,
        partition: None,
    }
}

#[tokio::test(start_paused = true)]
async fn whisper_drops_a_lone_credit_only_from_a_recording_under_a_second_of_sound()
-> anyhow::Result<()> {
    for (engine, audible, inserted) in [
        (Engine::Whisper, BRIEF, false),
        (Engine::Whisper, Duration::from_secs(1), true),
        (Engine::Nemo, BRIEF, true),
    ] {
        let mut h = Harness::with_ports(Config { engine, ..config() }, |ports| {
            ports.delayed_finish = true;
        })?;
        let (id, events) = h.start().await?;
        events
            .send(CaptureEvent::Finished(id, Ok(Some(heard_for(audible)))))
            .await?;
        h.transcribe_next_as("Thank you.").await?;
        if inserted {
            assert_eq!(receive(&h.pasted).await?, "Thank you.");
            h.phase(Phase::Done).await?;
        } else {
            h.phase(Phase::Empty).await?;
            assert!(
                h.pasted.try_recv().is_err(),
                "An invented credit was pasted"
            );
        }
        h.close().await?;
    }
    Ok(())
}
