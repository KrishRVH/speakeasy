//! Continuous inference remains optional work: cancellation, promotion, and a busy worker keep
//! session authority and complete sealed audio.

use zeroize::Zeroizing;

use super::*;
use crate::{models::Model, runtime::ports::LiveAudio};

fn config() -> Config {
    Config {
        engine: crate::config::Engine::Parakeet,
        nemo_model: Some(Model::Nemotron),
        model: Model::Nemotron.file_name().into(),
        use_gpu: false,
        ..Config::default()
    }
}

struct Feed {
    frames: tokio::sync::mpsc::Sender<Zeroizing<Vec<u8>>>,
    ending: oneshot::Sender<Zeroizing<Vec<u8>>>,
}

fn live() -> (Feed, LiveAudio) {
    let (frames, received) = tokio::sync::mpsc::channel(1);
    let (ending, tail) = oneshot::channel();
    (
        Feed { frames, ending },
        LiveAudio {
            rate: 16_000,
            frames: received,
            tail,
        },
    )
}

#[tokio::test(start_paused = true)]
async fn cancellation_closes_the_stream_and_reuses_a_responsive_cpu_worker() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(config(), |_| {})?;
    receive(&h.loads).await?;
    let (id, events) = h.start().await?;
    let (feed, audio) = live();
    events.send(CaptureEvent::Streaming(id, audio)).await?;
    // An owner round trip admits the stream before the cancellation arrives.
    events.send(CaptureEvent::Level(id, 0.5)).await?;
    h.observe_level(0.5).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    timeout(PATIENCE, feed.frames.closed()).await?;
    h.observe(|snapshot| snapshot.model == ModelState::Ready)
        .await?;
    h.start().await?;
    h.finish_as("Current recording").await?;
    assert_eq!(receive(&h.pasted).await?, "Current recording");
    assert!(
        h.loads.try_recv().is_err(),
        "Cancellation replaced a healthy streaming worker"
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn abandoning_stream_for_show_keeps_the_worker_and_whole_narration() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(config(), |ports| ports.delayed_finish = true)?;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    let (feed, audio) = live();
    events.send(CaptureEvent::Streaming(id, audio)).await?;
    h.input(Input::Show(Scope::Screen));
    receive(&h.shows).await?;
    drop(feed);
    events
        .send(CaptureEvent::Finished(id, Ok(Some(sealed()))))
        .await?;
    h.transcribe_next_as("Whole Show narration").await?;
    let (_, narration) = receive(&h.published).await?;
    assert_eq!(narration, Narration::Text("Whole Show narration".into()));
    assert!(h.loads.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn stream_output_waits_for_seal_and_keeps_practice_private() -> anyhow::Result<()> {
    let mut h = Harness::with_ports(config(), |ports| ports.delayed_finish = true)?;
    let mut practice = h.runtime.practice().map_err(anyhow::Error::msg)?;
    assert!(matches!(
        timeout(PATIENCE, practice.recv()).await?,
        Some(PracticeEvent::Ready)
    ));
    let (id, events) = h.start().await?;
    let (feed, audio) = live();
    events.send(CaptureEvent::Streaming(id, audio)).await?;
    drop(feed.frames);
    assert!(feed.ending.send(Zeroizing::new(Vec::new())).is_ok());
    h.transcribe_next_as("Private streamed result").await?;
    assert!(still_pending(practice.recv()).await);
    events
        .send(CaptureEvent::Finished(id, Ok(Some(sealed()))))
        .await?;
    let Some(PracticeEvent::Text(text)) = timeout(PATIENCE, practice.recv()).await? else {
        panic!("Missing practice result")
    };
    assert_eq!(&*text, "Private streamed result");
    h.phase(Phase::Done).await?;
    assert!(h.jobs.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn recording_while_worker_recovers_skips_stream_and_preserves_whole_audio()
-> anyhow::Result<()> {
    let (recovered, probe) = async_channel::bounded(1);
    let (recorded, requests) = async_channel::bounded(1);
    let mut h = Harness::with_ports(config(), |ports| {
        ports.recovery = Some(probe);
        ports.recorded_requests = Some(recorded);
    })?;
    let (id, events) = h.start().await?;
    let (old, audio) = live();
    events.send(CaptureEvent::Streaming(id, audio)).await?;
    events.send(CaptureEvent::Level(id, 0.5)).await?;
    h.observe_level(0.5).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    timeout(PATIENCE, old.frames.closed()).await?;
    let (id, events) = h.start().await?;
    let (new, audio) = live();
    events.send(CaptureEvent::Streaming(id, audio)).await?;
    timeout(PATIENCE, new.frames.closed()).await?;
    recovered.send(Ok(())).await?;
    h.finish_as("Complete recording").await?;
    assert_eq!(receive(&requests).await?, sealed().wav);
    assert_eq!(receive(&h.pasted).await?, "Complete recording");
    h.close().await
}
