//! Opt-in profiling of the real owner and local speech with public audio and fake
//! capture/insertion. It never installs a hook or touches an input device.

use std::{path::PathBuf, time::Duration};

use speakeasy_platform::{Delivery, InsertPermit, Inserted, Taps};
use tokio::time::timeout;

use super::{
    ports::{Capturing, Listening, Pinning, Sealed, Speech},
    *,
};
use crate::{
    local_speech::{self, LocalSpeech},
    recognition_terms::Terms,
};

/// Outlasts both of the worker's startup bounds, one to serve and one to warm up.
const STARTUP_TIMEOUT: Duration = local_speech::STARTUP_TIMEOUT
    .saturating_mul(2)
    .saturating_add(Duration::from_secs(5));
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const PHASE_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

struct Fixture {
    wav: Vec<u8>,
    loaded: Sender<()>,
    inserted: Sender<(Instant, String)>,
    replay: Option<Replay>,
    requests: Option<Sender<Inference>>,
}

impl Ports for Fixture {
    type Microphone = FixtureDevice;
    type Screen = FixtureDevice;
    type Speech = MeasuredSpeech;

    fn prepares_desktop(&self) -> bool {
        false
    }

    fn record_microphone(
        &self,
        id: SessionId,
        _: Option<&str>,
        recognition: crate::runtime::ports::RecognitionMode,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<FixtureDevice> {
        let capture = self
            .replay
            .as_ref()
            .map(|replay| {
                crate::audio::fixture::replay(
                    id,
                    &self.wav,
                    if replay.segmenting {
                        recognition
                    } else {
                        crate::runtime::ports::RecognitionMode::Whole
                    },
                    replay.speed,
                    events.clone(),
                    replay.finished.clone(),
                    replay.statistics.clone(),
                )
            })
            .transpose()?;
        if capture.is_none() {
            events.try_send(CaptureEvent::Ready(id))?;
        }
        Ok(FixtureDevice {
            capture,
            id,
            wav: self.wav.clone(),
            events,
        })
    }

    fn keep(&self, _: std::time::SystemTime) -> anyhow::Result<Option<crate::show::Kept>> {
        Ok(None)
    }

    fn record_screen(
        &self,
        _: SessionId,
        _: Instant,
        _: Scope,
        _: Option<crate::show::FrameLog>,
        _: Sender<ShowEvent>,
    ) -> anyhow::Result<FixtureDevice> {
        anyhow::bail!("The dictation fixture records no Shows")
    }

    fn reopen(
        &self,
        interrupted: crate::show::Interrupted,
    ) -> impl Future<Output = anyhow::Result<crate::show::Reopened>> + Send + 'static {
        crate::show::reopen(interrupted)
    }

    fn publish(
        &self,
        _: crate::show::Publication,
        _: InsertPermit,
    ) -> impl Future<Output = anyhow::Result<Published>> + Send + 'static {
        std::future::ready(Err(anyhow::anyhow!(
            "The dictation fixture publishes no Shows"
        )))
    }

    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<MeasuredSpeech>> + Send + 'static {
        let loaded = self.loaded.clone();
        let requests = self.requests.clone();
        async move {
            let worker = LocalSpeech::start(config, cancelled).await?;
            loaded.send(()).await?;
            Ok(MeasuredSpeech { worker, requests })
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
            inserted.try_send((Instant::now(), text))?;
            Ok(Inserted::Sent)
        }
    }

    fn cue(&self, _: Cue) {}

    fn hold_cues(&self, _: bool) {}
}

/// Replays the fixture audio when finished; a repeated finish reaches a session that is no longer
/// capturing and is ignored.
struct FixtureDevice {
    capture: Option<crate::audio::Capture>,
    id: SessionId,
    wav: Vec<u8>,
    events: Sender<CaptureEvent>,
}

impl Capturing for FixtureDevice {
    fn finish(&self) {
        if let Some(capture) = &self.capture {
            capture.finish();
            return;
        }
        let finished = CaptureEvent::Finished(
            self.id,
            Ok(Some(Sealed {
                wav: self.wav.clone(),
                trimmed: Duration::ZERO,
                partition: None,
            })),
        );
        self.events
            .try_send(finished)
            .expect("Capture lane has room for fixture audio");
    }

    async fn retire(self) {
        if let Some(capture) = self.capture {
            capture.retire().await;
        }
    }
}

impl Listening for FixtureDevice {
    fn whole(&self) {
        if let Some(capture) = &self.capture {
            capture.whole();
        }
    }

    fn keep(&self, _: crate::show::AudioLog, _: Duration, _: &str) -> bool {
        true
    }

    fn expect_cue(&self) {}
}

/// No fixture device records a Show, so none takes a pin.
impl Pinning for FixtureDevice {
    fn pin(&self, _: Taps) {}
}

async fn wait_for_signal<T: Send>(
    signal: &Receiver<T>,
    updates: &mut watch::Receiver<Snapshot>,
    limit: Duration,
) -> anyhow::Result<T> {
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
    let path = PathBuf::from(std::env::var("SPEAKEASY_FIXTURE_CONFIG")?);
    let config = Config::read(&path)?.validated(&path)?.into_inner();
    let wav = std::fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
    let (loaded, ready) = async_channel::bounded(1);
    let (inserted, submitted) = async_channel::bounded(1);
    let (output, mut updates) = watch::channel(Snapshot::default());
    let (meter, _) = watch::channel(Meter::default());
    let started = Instant::now();
    let fixture = Fixture {
        wav,
        loaded,
        inserted,
        replay: None,
        requests: None,
    };
    let runtime = Runtime::start_with(config, output, meter, fixture)?;
    wait_for_signal(&ready, &mut updates, STARTUP_TIMEOUT).await?;
    let startup_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut requests_ms = Vec::new();
    for _ in 0..5 {
        runtime.input.deliver(Input::Toggle);
        wait_for_phase(&mut updates, Phase::Recording).await?;
        let requested = Instant::now();
        runtime.input.deliver(Input::Toggle);
        let (_inserted_at, _checked_text) =
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

struct Replay {
    speed: u32,
    segmenting: bool,
    finished: Sender<Instant>,
    statistics: Sender<crate::audio::fixture::Statistics>,
}

#[derive(serde::Serialize)]
struct Inference {
    audio_ms: f64,
    elapsed_ms: f64,
    sha256: String,
}

struct MeasuredSpeech {
    worker: LocalSpeech,
    requests: Option<Sender<Inference>>,
}

impl Speech for MeasuredSpeech {
    async fn transcribe_stream(
        &mut self,
        audio: crate::runtime::ports::LiveAudio,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<Option<String>> {
        self.worker.transcribe_stream(audio, language, terms).await
    }

    async fn transcribe(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<String> {
        let measurement = self.requests.as_ref().map(|_| {
            let rate = crate::audio::wav_sample_rate(&wav).unwrap_or(1).max(1);
            (
                wav.len().saturating_sub(44) as f64 * 500.0 / f64::from(rate),
                sha256(&wav),
            )
        });
        let started = Instant::now();
        let text = self.worker.transcribe(wav, language, terms).await?;
        if let Some(((audio_ms, sha256), reports)) = measurement.zip(self.requests.as_ref()) {
            reports.try_send(Inference {
                audio_ms,
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                sha256,
            })?;
        }
        Ok(text)
    }

    async fn transcribe_words(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<speakeasy_show::Narration> {
        self.worker.transcribe_words(wav, language, terms).await
    }

    async fn probe_with_silence(&mut self) -> anyhow::Result<()> {
        self.worker.probe_with_silence().await
    }

    async fn stop(&mut self) {
        self.worker.stop().await;
    }
}

#[tokio::test]
#[ignore = "Public PCM replay with real inference; requires fixture config, canonical WAV, and a public reference text file"]
async fn profile_recording_dictation() -> anyhow::Result<()> {
    let path = PathBuf::from(std::env::var("SPEAKEASY_FIXTURE_CONFIG")?);
    let config = Config::read(&path)?.validated(&path)?.into_inner();
    let wav = std::fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
    let reference = std::fs::read_to_string(std::env::var("SPEAKEASY_FIXTURE_REFERENCE")?)?;
    let speed = std::env::var("SPEAKEASY_FIXTURE_SPEED")
        .unwrap_or_else(|_| "1".into())
        .parse::<u32>()?;
    let segmenting = std::env::var("SPEAKEASY_FIXTURE_SEGMENT").is_ok_and(|value| value == "1");
    let continuous = segmenting && config.uses_nemotron();
    let (loaded, ready) = async_channel::bounded(1);
    let (inserted, submitted) = async_channel::bounded(1);
    let (finished, ended) = async_channel::bounded(1);
    let (statistics, drained) = async_channel::bounded(1);
    let (requests, measurements) = async_channel::unbounded();
    let (output, mut updates) = watch::channel(Snapshot::default());
    let (meter, _) = watch::channel(Meter::default());
    let fixture = Fixture {
        wav,
        loaded,
        inserted,
        replay: Some(Replay {
            speed,
            segmenting,
            finished,
            statistics,
        }),
        requests: Some(requests),
    };
    let runtime = Runtime::start_with(config, output, meter, fixture)?;
    wait_for_signal(&ready, &mut updates, STARTUP_TIMEOUT).await?;
    runtime.input.deliver(Input::Toggle);
    let stopped = wait_for_signal(&ended, &mut updates, Duration::from_secs(310)).await?;
    let (inserted, text) = wait_for_signal(&submitted, &mut updates, REQUEST_TIMEOUT).await?;
    let stats = timeout(PHASE_TIMEOUT, drained.recv()).await??;
    wait_for_phase(&mut updates, Phase::Done).await?;
    runtime.request_stop();
    timeout(SHUTDOWN_TIMEOUT, runtime.stopped()).await?;
    let requests: Vec<_> = std::iter::from_fn(|| measurements.try_recv().ok()).collect();
    let words = |text: &str| {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    let expected = words(&reference);
    let heard = words(&text);
    let word_errors = distance(&expected, &heard);
    let spelling_errors = distance(
        &reference.split_whitespace().collect::<Vec<_>>(),
        &text.split_whitespace().collect::<Vec<_>>(),
    );
    eprintln!(
        "Recording fixture profile: {}",
        serde_json::json!({
            "segmenting": segmenting,
            "continuous": continuous,
            "speed": speed,
            "stop_to_fake_insert_ms": inserted.saturating_duration_since(stopped).as_secs_f64() * 1000.0,
            "requests": requests,
            "capture": stats,
            "reference_words": expected.len(),
            "heard_words": heard.len(),
            "word_errors": word_errors,
            "formatting_word_errors": spelling_errors,
            "text_sha256": sha256(text.as_bytes()),
            "total_inference_ms": (!continuous).then(|| requests.iter().map(|request| request.elapsed_ms).sum::<f64>()),
        })
    );
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("String formatting cannot fail");
            hex
        })
}

fn distance<T: PartialEq>(expected: &[T], heard: &[T]) -> usize {
    let mut row: Vec<_> = (0..=heard.len()).collect();
    for (i, word) in expected.iter().enumerate() {
        let mut previous = row.first().copied().unwrap_or(0);
        if let Some(first) = row.first_mut() {
            *first = i.saturating_add(1);
        }
        let mut left = i.saturating_add(1);
        for (cell, actual) in row.iter_mut().skip(1).zip(heard) {
            let above = *cell;
            *cell = above
                .saturating_add(1)
                .min(left.saturating_add(1))
                .min(previous.saturating_add(usize::from(word != actual)));
            previous = above;
            left = *cell;
        }
    }
    row.last().copied().unwrap_or(expected.len())
}
