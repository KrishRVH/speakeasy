//! Early inference preserves session authority, complete output, and whole-recording fallback.

use super::*;
use crate::{config::Engine, runtime::ports::Partition, transcript::vocabulary::Correction};

fn config() -> Config {
    Config {
        engine: Engine::Parakeet,
        use_gpu: false,
        ..Config::default()
    }
}

#[tokio::test(start_paused = true)]
async fn segmentation_admits_only_parakeet_dictation_regardless_of_legacy_language()
-> anyhow::Result<()> {
    for (engine, language, show, expected) in [
        (Engine::Parakeet, "en", false, true),
        (Engine::Parakeet, "auto", false, true),
        (Engine::Whisper, "en", false, false),
        (Engine::Whisper, "auto", false, false),
        (Engine::Parakeet, "auto", true, false),
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

fn partitioned(pieces: u32) -> anyhow::Result<Sealed> {
    Ok(Sealed {
        wav: crate::audio::silent_wav(16_000, Duration::from_secs(1))?,
        trimmed: Duration::ZERO,
        partition: Some(Partition {
            pieces,
            tail: 32_000..32_000,
        }),
    })
}

#[tokio::test(start_paused = true)]
async fn pieces_clean_fillers_and_corrections_once_after_recording_stops() -> anyhow::Result<()> {
    let mut config = config();
    config.vocabulary = vec![Correction {
        heard: "cube control".into(),
        written: "kubectl".into(),
    }];
    let mut h = Harness::with_ports(config, |ports| ports.delayed_finish = true)?;
    let (id, events) = h.start().await?;
    for (number, text) in [(0, "Ready,"), (1, "um. cube"), (2, "control")] {
        events
            .send(CaptureEvent::Piece(id, number, sealed()))
            .await?;
        h.transcribe_next_as(text).await?;
    }
    assert!(
        still_pending(h.pasted.recv()).await,
        "Text reached insertion during capture"
    );
    events
        .send(CaptureEvent::Finished(id, Ok(Some(partitioned(3)?))))
        .await?;
    assert_eq!(receive(&h.pasted).await?, "Ready. kubectl");
    h.phase(Phase::Done).await?;
    assert!(
        h.jobs.try_recv().is_err(),
        "A quiet tail asked for another recognition"
    );
    h.close().await
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
            .send(CaptureEvent::Piece(id, number, sealed()))
            .await?;
        h.transcribe_next_as(text).await?;
        assert_eq!(receive(&requests).await?, sealed().wav);
    }
    assert!(still_pending(h.pasted.recv()).await);
    let mut audio = crate::audio::silent_wav(16_000, Duration::from_secs(2))?;
    audio[44..32_044].fill(0x55);
    audio[32_044..].fill(0x2A);
    events
        .send(CaptureEvent::Finished(
            id,
            Ok(Some(Sealed {
                wav: audio.clone(),
                trimmed: Duration::ZERO,
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
    assert_eq!(crate::audio::wav_sample_rate(&tail)?, 16_000);
    assert!(still_pending(h.pasted.recv()).await);
    answer(reply, "control")?;
    assert_eq!(receive(&h.pasted).await?, "Ready. kubectl");
    h.phase(Phase::Done).await?;
    assert!(h.pasted.try_recv().is_err());
    assert!(h.jobs.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn a_failed_detached_piece_replaces_its_worker_while_idle() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(config(), |_| {})?;
    receive(&h.loads).await?;
    let (id, events) = h.start().await?;
    events.send(CaptureEvent::Piece(id, 0, sealed())).await?;
    let (_, reply) = receive(&h.jobs).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    assert!(h.loads.try_recv().is_err());
    assert!(reply.send(Err(anyhow!("Detached piece failed"))).is_ok());
    receive(&h.loads).await?;
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await?;
    assert_eq!(h.snapshot().phase, Phase::Cancelled);
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn changing_session_settings_during_a_piece_does_not_claim_model_loading()
-> anyhow::Result<()> {
    let config = config();
    let mut h = Harness::with_ports(config.clone(), |_| {})?;
    let (id, events) = h.start().await?;
    events.send(CaptureEvent::Piece(id, 0, sealed())).await?;
    let (_, reply) = receive(&h.jobs).await?;
    let mut changed = config;
    changed.remove_fillers = !changed.remove_fillers;
    h.runtime.configure(ValidConfig::assume(changed));
    h.phase(Phase::Cancelled).await?;
    assert_eq!(h.snapshot().message, "Settings applied");
    answer(reply, "stale")?;
    h.start().await?;
    h.finish_as("current").await?;
    assert_eq!(receive(&h.pasted).await?, "current");
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn stale_piece_success_and_failure_cannot_reach_the_next_session() -> anyhow::Result<()> {
    for fails in [false, true] {
        let mut h = Harness::with_ports(config(), |_| {})?;
        receive(&h.loads).await?;
        let (id, events) = h.start().await?;
        events.send(CaptureEvent::Piece(id, 0, sealed())).await?;
        let (_, reply) = receive(&h.jobs).await?;
        h.input(Input::Cancel);
        h.phase(Phase::Cancelled).await?;
        h.start().await?;
        assert!(
            h.loads.try_recv().is_err(),
            "Cancelling a piece reloaded the worker"
        );
        assert!(
            reply
                .send(if fails {
                    Err(anyhow!("Old piece failed"))
                } else {
                    Ok("stale".into())
                })
                .is_ok(),
            "A piece was aborted instead of draining"
        );
        h.finish_as("current").await?;
        assert_eq!(receive(&h.pasted).await?, "current");
        h.phase(Phase::Done).await?;
        assert!(h.pasted.try_recv().is_err());
        if !fails {
            assert!(
                h.loads.try_recv().is_err(),
                "A healthy drained worker was replaced"
            );
        }
        h.close().await?;
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_failed_piece_replaces_its_worker_but_waits_for_seal_to_fall_back() -> anyhow::Result<()>
{
    let mut h = Harness::with_ports(config(), |ports| ports.delayed_finish = true)?;
    receive(&h.loads).await?;
    let (id, events) = h.start().await?;
    events.send(CaptureEvent::Piece(id, 0, sealed())).await?;
    let (_, reply) = receive(&h.jobs).await?;
    assert!(reply.send(Err(anyhow!("Piece failed"))).is_ok());
    receive(&h.loads).await?;
    assert!(
        still_pending(h.jobs.recv()).await,
        "Fallback recognized unfinished audio"
    );
    events
        .send(CaptureEvent::Finished(id, Ok(Some(partitioned(1)?))))
        .await?;
    h.transcribe_next_as("whole recording").await?;
    assert_eq!(receive(&h.pasted).await?, "whole recording");
    h.phase(Phase::Done).await?;
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn promotion_during_a_piece_uses_whole_show_narration() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(config(), |ports| ports.delayed_finish = true)?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    events.send(CaptureEvent::Piece(id, 0, sealed())).await?;
    let (_, reply) = receive(&h.jobs).await?;
    h.input(Input::Show(Scope::Screen));
    receive(&h.shows).await?;
    answer(reply, "piece must not become narration")?;
    events
        .send(CaptureEvent::Finished(id, Ok(Some(partitioned(1)?))))
        .await?;
    h.transcribe_next_as("whole Show narration").await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("whole Show narration".into()));
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}
