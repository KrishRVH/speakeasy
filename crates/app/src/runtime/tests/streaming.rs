use super::*;
use crate::{config::Engine, models::Model, runtime::ports::LiveAudio};

fn config() -> Config {
    Config {
        engine: Engine::Nemo,
        nemo_model: Some(Model::Nemotron),
        model: Model::Nemotron.file_name().into(),
        use_gpu: false,
        ..Config::default()
    }
}

struct Feed {
    frames: mpsc::Sender<Vec<u8>>,
    _ending: oneshot::Sender<Vec<u8>>,
}

fn live() -> (Feed, LiveAudio) {
    let (frames, received) = mpsc::channel(1);
    let (ending, tail) = oneshot::channel();
    (
        Feed {
            frames,
            _ending: ending,
        },
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
