//! Opt-in profiling of the real controller/provider with public audio and fake
//! capture/insertion. This never installs a hook or touches an input device.
use super::*;
use crate::local_speech::LocalSpeech;
use async_channel::Sender;
use speakeasy_platform::Inserted;

struct Fixture {
    wav: Vec<u8>,
    loaded: Sender<()>,
    inserted: Sender<()>,
}

struct FixtureRecording {
    id: SessionId,
    #[expect(
        clippy::disallowed_types,
        reason = "The capture port signals finish through &self; this single-owner fake must take its public fixture audio exactly once"
    )]
    wav: std::cell::RefCell<Option<Vec<u8>>>,
    events: Sender<Event>,
}

async fn wait_for_event(
    signal: &Receiver<()>,
    updates: &mut watch::Receiver<Snapshot>,
    limit: Duration,
) -> anyhow::Result<()> {
    tokio::time::timeout(limit, async {
        tokio::select! {
            result = signal.recv() => Ok(result?),
            failed = updates.wait_for(|s| s.phase == Phase::Error) => {
                anyhow::bail!("Fixture controller failed: {}", failed?.message);
            }
        }
    })
    .await?
}

impl Recording for FixtureRecording {
    async fn retire(self) {}
    fn finish(&self) {
        let wav = self.wav.borrow_mut().take();
        assert!(
            self.events
                .try_send(Event::AudioDone(self.id, Ok(wav)))
                .is_ok()
        );
    }
}

impl Ports for Fixture {
    fn prepares_desktop(&self) -> bool {
        false
    }
    type Recording = FixtureRecording;
    type Speech = LocalSpeech;

    fn record(
        &self,
        id: SessionId,
        _: Option<String>,
        events: Sender<Event>,
    ) -> anyhow::Result<Self::Recording> {
        events.try_send(Event::Ready(id))?;
        #[expect(
            clippy::disallowed_types,
            reason = "Only this fake capture owns the fixture buffer; finish takes it once through the production &self port"
        )]
        let wav = std::cell::RefCell::new(Some(self.wav.clone()));
        Ok(FixtureRecording { id, wav, events })
    }

    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl std::future::Future<Output = anyhow::Result<LocalSpeech>> + Send + 'static {
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
        gate: speakeasy_platform::InsertPermit,
        _: bool,
    ) -> impl std::future::Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let inserted = self.inserted.clone();
        async move {
            if !text.to_lowercase().contains("ask not what your country") {
                return Ok(Inserted::Unavailable("Fixture phrase missing"));
            }
            if text.contains(['\r', '\n']) {
                return Ok(Inserted::Unavailable("Segment layout reached insertion"));
            }
            if !gate.commit() {
                return Ok(Inserted::Unavailable("Fixture insertion was cancelled"));
            }
            inserted.try_send(())?;
            Ok(Inserted::Sent)
        }
    }
}

#[tokio::test]
#[ignore = "Profiles real local inference; requires SPEAKEASY_FIXTURE_CONFIG and SPEAKEASY_FIXTURE_WAV"]
async fn profile_fixture_dictation() -> anyhow::Result<()> {
    let config = Config::load(std::path::Path::new(&std::env::var(
        "SPEAKEASY_FIXTURE_CONFIG",
    )?))?;
    let wav = std::fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
    let (loaded, ready) = async_channel::bounded(1);
    let (inserted, submitted) = async_channel::bounded(1);
    let (output, mut updates) = watch::channel(Snapshot::default());
    let started = Instant::now();
    let runtime = Runtime::start_with(
        config,
        output,
        Fixture {
            wav,
            loaded,
            inserted,
        },
    )?;
    wait_for_event(&ready, &mut updates, Duration::from_secs(245)).await?;
    let startup_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut requests_ms = Vec::new();
    for _ in 0..5 {
        speakeasy_platform::deliver(&runtime.input, Input::Toggle);
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            updates.wait_for(|s| s.phase == Phase::Recording),
        )
        .await??;
        let started = Instant::now();
        speakeasy_platform::deliver(&runtime.input, Input::Toggle);
        wait_for_event(&submitted, &mut updates, Duration::from_secs(60)).await?;
        requests_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            updates.wait_for(|s| s.phase == Phase::Done),
        )
        .await??;
    }
    let stopped = Instant::now();
    runtime.request_stop();
    tokio::time::timeout(Duration::from_secs(5), runtime.stopped()).await?;
    eprintln!(
        "Fixture controller profile: {}",
        serde_json::json!({
            "startup_ms": startup_ms,
            "stop_to_fake_insert_ms": requests_ms,
            "shutdown_ms": stopped.elapsed().as_secs_f64() * 1000.0,
        })
    );
    Ok(())
}
