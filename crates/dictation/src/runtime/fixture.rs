//! Opt-in profiling of the real owner and local speech with public audio and fake
//! capture/insertion. It never installs a hook or touches an input device.

use std::{path::Path, time::Duration};

use speakeasy_platform::{Delivery, InsertPermit, Inserted};
use tokio::time::timeout;

use super::*;
use crate::{local_speech::LocalSpeech, ports::Recording};

/// Outlasts the worker's own startup bounds, 120 s to serve and 120 s to warm up, by 5 s.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(2 * 120 + 5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const PHASE_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

struct Fixture {
    wav: Vec<u8>,
    loaded: Sender<()>,
    inserted: Sender<()>,
}

impl Ports for Fixture {
    type Recording = FixtureRecording;
    type Speech = LocalSpeech;

    fn prepares_desktop(&self) -> bool {
        false
    }

    fn record(
        &self,
        id: SessionId,
        _: Option<&str>,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<FixtureRecording> {
        events.try_send(CaptureEvent::Ready(id, Duration::ZERO))?;
        Ok(FixtureRecording {
            id,
            wav: self.wav.clone(),
            events,
        })
    }

    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<LocalSpeech>> + Send + 'static {
        let loaded = self.loaded.clone();
        async move {
            let worker = LocalSpeech::start(config, cancelled).await?;
            loaded.send(()).await?;
            Ok(worker)
        }
    }

    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        _: Delivery,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let inserted = self.inserted.clone();
        async move {
            if !text.to_lowercase().contains("ask not what your country") {
                return Ok(Inserted::Unavailable("Fixture phrase missing"));
            }
            if text.contains(['\r', '\n']) {
                return Ok(Inserted::Unavailable("Segment layout reached insertion"));
            }
            if !permit.commit() {
                return Ok(Inserted::Unavailable("Fixture insertion was cancelled"));
            }
            inserted.try_send(())?;
            Ok(Inserted::Sent)
        }
    }
}

/// Replays the fixture audio when finished; a repeated finish reaches a session that is no longer
/// capturing and is ignored.
struct FixtureRecording {
    id: SessionId,
    wav: Vec<u8>,
    events: Sender<CaptureEvent>,
}

impl Recording for FixtureRecording {
    fn finish(&self) {
        let finished = CaptureEvent::Finished(self.id, Ok(Some(self.wav.clone())));
        self.events
            .try_send(finished)
            .expect("capture lane has room for fixture audio");
    }

    async fn retire(self) {}
}

async fn wait_for_signal(
    signal: &Receiver<()>,
    updates: &mut watch::Receiver<Snapshot>,
    limit: Duration,
) -> anyhow::Result<()> {
    timeout(limit, async {
        tokio::select! {
            result = signal.recv() => Ok(result?),
            failed = updates.wait_for(|snapshot| snapshot.phase == Phase::Error) => {
                anyhow::bail!("Fixture owner failed: {}", failed?.message);
            }
        }
    })
    .await?
}

async fn wait_for_phase(
    updates: &mut watch::Receiver<Snapshot>,
    phase: Phase,
) -> anyhow::Result<()> {
    timeout(
        PHASE_TIMEOUT,
        updates.wait_for(|snapshot| snapshot.phase == phase),
    )
    .await??;
    Ok(())
}

#[tokio::test]
#[ignore = "Profiles real local inference; requires SPEAKEASY_FIXTURE_CONFIG and SPEAKEASY_FIXTURE_WAV"]
async fn profile_fixture_dictation() -> anyhow::Result<()> {
    let config = Config::load(Path::new(&std::env::var("SPEAKEASY_FIXTURE_CONFIG")?))?;
    let wav = std::fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
    let (loaded, ready) = async_channel::bounded(1);
    let (inserted, submitted) = async_channel::bounded(1);
    let (output, mut updates) = watch::channel(Snapshot::default());
    let started = Instant::now();
    let fixture = Fixture {
        wav,
        loaded,
        inserted,
    };
    let runtime = Runtime::start_with(config, output, fixture)?;
    wait_for_signal(&ready, &mut updates, STARTUP_TIMEOUT).await?;
    let startup_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut requests_ms = Vec::new();
    for _ in 0..5 {
        runtime.input.deliver(Input::Toggle);
        wait_for_phase(&mut updates, Phase::Recording).await?;
        let requested = Instant::now();
        runtime.input.deliver(Input::Toggle);
        wait_for_signal(&submitted, &mut updates, REQUEST_TIMEOUT).await?;
        requests_ms.push(requested.elapsed().as_secs_f64() * 1000.0);
        wait_for_phase(&mut updates, Phase::Done).await?;
    }
    let stopped = Instant::now();
    runtime.request_stop();
    timeout(SHUTDOWN_TIMEOUT, runtime.stopped()).await?;
    eprintln!(
        "Fixture owner profile: {}",
        serde_json::json!({
            "startup_ms": startup_ms,
            "stop_to_fake_insert_ms": requests_ms,
            "shutdown_ms": stopped.elapsed().as_secs_f64() * 1000.0,
        })
    );
    Ok(())
}
