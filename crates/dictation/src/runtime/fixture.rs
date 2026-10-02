//! Opt-in profiling of the real owner and local speech with public audio and fake
//! capture/insertion. It never installs a hook or touches an input device.

use std::{path::Path, time::Duration};

use speakeasy_platform::{Delivery, InsertPermit, Inserted};
use tokio::time::timeout;

use super::*;
use crate::{audio::Replay, local_speech::LocalSpeech, ports::Recording};

/// Outlasts the worker's own startup bounds, to serve and to warm up, by 5 s.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(2 * 120 + 5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const PHASE_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

struct Fixture {
    wav: Vec<u8>,
    loaded: Sender<()>,
    inserted: Sender<()>,
    /// Replays the audio in real time, with or without speculation, reporting when its last word
    /// was fed; otherwise the whole recording arrives at the finish.
    replay: Option<(bool, Sender<Instant>)>,
    timings: Option<Sender<String>>,
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
        if let Some((speculation, spoken)) = &self.replay {
            return Ok(FixtureRecording::Replay(Replay::start(
                id,
                &self.wav,
                *speculation,
                events,
                spoken.clone(),
            )?));
        }
        events.try_send(CaptureEvent::Ready(id, Duration::ZERO))?;
        Ok(FixtureRecording::Whole {
            id,
            wav: self.wav.clone(),
            events,
        })
    }

    fn report_timing(&self, line: String) {
        if let Some(timings) = &self.timings {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The profile reads only the timings it waits for; a full lane drops the rest"
            )]
            let _ = timings.try_send(line);
        }
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
        let any_text = self.replay.is_some();
        async move {
            if !any_text && !text.to_lowercase().contains("ask not what your country") {
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

/// The fixture audio: whole when finished, where a repeated finish reaches a session that is no
/// longer capturing and is ignored, or replayed in real time.
enum FixtureRecording {
    Whole {
        id: SessionId,
        wav: Vec<u8>,
        events: Sender<CaptureEvent>,
    },
    Replay(Replay),
}

impl Recording for FixtureRecording {
    fn finish(&self) {
        match self {
            Self::Whole { id, wav, events } => {
                let captured = Captured {
                    wav: Some(wav.clone()),
                    standing: Vec::new(),
                    segments: 0,
                };
                events
                    .try_send(CaptureEvent::Finished(*id, Ok(Some(captured))))
                    .expect("capture lane has room for fixture audio");
            },
            Self::Replay(replay) => replay.finish(),
        }
    }

    fn speculate(&self) {
        if let Self::Replay(replay) = self {
            replay.speculate();
        }
    }

    async fn retire(self) {
        if let Self::Replay(replay) = self {
            replay.retire().await;
        }
    }
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
#[ignore = "Profiles real local inference; requires SPEAKEASY_FIXTURE_CONFIG, SPEAKEASY_FIXTURE_WAV, and for Parakeet SPEAKEASY_FIXTURE_HELPER"]
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
        replay: None,
        timings: None,
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

/// Times a held dictation as a person makes it: the audio plays in real time from the press, and
/// the shortcut is released `SPEAKEASY_FIXTURE_RELEASE_MS` (default 300) after the last word.
/// `SPEAKEASY_FIXTURE_NO_SPECULATION` turns pause speculation off, so each recording makes its own
/// request.
#[tokio::test]
#[ignore = "Profiles real local inference in real time; requires SPEAKEASY_FIXTURE_CONFIG, SPEAKEASY_FIXTURE_WAV, and for Parakeet SPEAKEASY_FIXTURE_HELPER"]
async fn profile_held_dictation() -> anyhow::Result<()> {
    let config = Config::load(Path::new(&std::env::var("SPEAKEASY_FIXTURE_CONFIG")?))?;
    let wav = std::fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
    let release = Duration::from_millis(
        std::env::var("SPEAKEASY_FIXTURE_RELEASE_MS").map_or(Ok(300), |ms| ms.parse())?,
    );
    let speculation = std::env::var_os("SPEAKEASY_FIXTURE_NO_SPECULATION").is_none();
    let runs: usize = std::env::var("SPEAKEASY_FIXTURE_RUNS").map_or(Ok(5), |runs| runs.parse())?;
    let (loaded, ready) = async_channel::bounded(1);
    let (inserted, submitted) = async_channel::bounded(1);
    let (spoken, last_word) = async_channel::bounded(1);
    let (timings, reports) = async_channel::unbounded();
    let (output, mut updates) = watch::channel(Snapshot::default());
    let fixture = Fixture {
        wav,
        loaded,
        inserted,
        replay: Some((speculation, spoken)),
        timings: Some(timings),
    };
    let runtime = Runtime::start_with(config, output, fixture)?;
    wait_for_signal(&ready, &mut updates, STARTUP_TIMEOUT).await?;
    let mut release_to_insert_ms = Vec::new();
    let mut speculated = 0;
    for _ in 0..runs {
        runtime.input.deliver(Input::Press);
        wait_for_phase(&mut updates, Phase::Recording).await?;
        let spoken_at = timeout(REQUEST_TIMEOUT, last_word.recv()).await??;
        tokio::time::sleep_until((spoken_at + release).into()).await;
        let released = Instant::now();
        runtime.input.deliver(Input::Release);
        wait_for_signal(&submitted, &mut updates, REQUEST_TIMEOUT).await?;
        release_to_insert_ms.push(released.elapsed().as_secs_f64() * 1000.0);
        wait_for_phase(&mut updates, Phase::Done).await?;
        let report = timeout(PHASE_TIMEOUT, reports.recv()).await??;
        speculated += usize::from(report.contains(" · speculated"));
        // Let the GPU idle as between real dictations.
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    runtime.request_stop();
    timeout(SHUTDOWN_TIMEOUT, runtime.stopped()).await?;
    eprintln!(
        "Held dictation profile: {}",
        serde_json::json!({
            "release_after_last_word_ms": release.as_secs_f64() * 1000.0,
            "speculation": speculation,
            "release_to_fake_insert_ms": release_to_insert_ms,
            "speculated": speculated,
        })
    );
    Ok(())
}
