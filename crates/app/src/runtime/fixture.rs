use std::{path::PathBuf, time::Duration};

use anyhow::Context as _;
use speakeasy_platform::{Delivery, Enter, InsertPermit, Inserted, Taps};
use speakeasy_show::Narration;
use tokio::time::timeout;

use super::{
    ports::{LiveAudio, Pinning, Speech},
    *,
};
use crate::{
    audio::{self, fixture::Statistics},
    local_speech::{self, LocalSpeech},
    recognition_terms::Terms,
};

mod corpus;
mod paired;

use corpus::Clip;

const STARTUP_TIMEOUT: Duration = local_speech::STARTUP_TIMEOUT
    .saturating_mul(2)
    .saturating_add(Duration::from_secs(5));
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const RECORDING_TIMEOUT: Duration = Duration::from_secs(310);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

struct Fixture {
    wav: Vec<u8>,
    loaded: Sender<()>,
    inserted: Sender<(Instant, String)>,
    replay: Replay,
}

impl Ports for Fixture {
    type Awake = ();
    type Microphone = Capture;
    type Screen = Capture;
    type Speech = MeasuredSpeech;

    fn prepares_desktop(&self) -> bool {
        false
    }

    fn keep_awake(&self, _: Keep) -> Option<Self::Awake> {
        None
    }

    fn record_microphone(
        &self,
        id: SessionId,
        _: Option<&str>,
        recognition: RecognitionMode,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Capture> {
        let replay = &self.replay;
        let recognition = if replay.segmenting {
            recognition
        } else {
            RecognitionMode::Whole
        };
        audio::fixture::replay(
            id,
            &self.wav,
            recognition,
            replay.speed,
            events,
            replay.finished.clone(),
            replay.statistics.clone(),
        )
    }

    fn keep(&self, _: SystemTime) -> anyhow::Result<Option<show::Kept>> {
        Ok(None)
    }

    fn record_screen(
        &self,
        _: SessionId,
        _: Instant,
        _: Scope,
        _: Option<show::FrameLog>,
        _: Sender<ShowEvent>,
    ) -> anyhow::Result<Capture> {
        anyhow::bail!("The dictation fixture records no Shows")
    }

    fn reopen(
        &self,
        interrupted: Interrupted,
    ) -> impl Future<Output = anyhow::Result<show::Reopened>> + Send + 'static {
        show::reopen(interrupted)
    }

    fn publish(
        &self,
        _: show::Publication,
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
        let inferences = self.replay.inferences.clone();
        async move {
            let worker = LocalSpeech::start(config, cancelled).await?;
            loaded.send(()).await?;
            Ok(MeasuredSpeech { worker, inferences })
        }
    }

    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        _: Delivery,
        _: Enter,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let inserted = self.inserted.clone();
        async move {
            if !permit.commit() {
                return Ok(Inserted::Cancelled);
            }
            inserted.try_send((Instant::now(), text))?;
            Ok(Inserted::Sent)
        }
    }

    fn cue(&self, _: Cue) {}

    fn hold_cues(&self, _: bool) {}
}

impl Pinning for Capture {
    fn pin(&self, _: Taps) {}
}

async fn wait_for_signal<T>(
    signal: &Receiver<T>,
    updates: &mut watch::Receiver<Snapshot>,
    limit: Duration,
) -> anyhow::Result<T> {
    timeout(limit, async {
        tokio::select! {
            result = signal.recv() => Ok(result?),
            ended = updates.wait_for(|snapshot| {
                matches!(snapshot.phase, Phase::Error | Phase::Cancelled)
            }) => Err(interruption(&*ended?)),
        }
    })
    .await?
}

fn interruption(snapshot: &Snapshot) -> anyhow::Error {
    if snapshot.phase == Phase::Cancelled {
        anyhow::anyhow!("Fixture session was cancelled")
    } else {
        anyhow::anyhow!("Fixture owner failed: {}", snapshot.message)
    }
}

struct Replay {
    speed: u32,
    segmenting: bool,
    finished: Sender<Instant>,
    statistics: Sender<Statistics>,
    inferences: Sender<Inference>,
}

#[derive(serde::Serialize)]
struct Inference {
    audio_ms: f64,
    elapsed_ms: f64,
    sha256: String,
}

struct MeasuredSpeech {
    worker: LocalSpeech,
    inferences: Sender<Inference>,
}

impl Speech for MeasuredSpeech {
    async fn transcribe_stream(
        &mut self,
        audio: LiveAudio,
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
        let audio_ms = milliseconds(audio::wav_duration(&wav)?);
        let sha256 = sha256(&wav);
        let started = Instant::now();
        let text = self.worker.transcribe(wav, language, terms).await?;
        self.inferences.try_send(Inference {
            audio_ms,
            elapsed_ms: milliseconds(started.elapsed()),
            sha256,
        })?;
        Ok(text)
    }

    async fn transcribe_words(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<Narration> {
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
#[ignore = "Public PCM replay with real inference; requires fixture config and a corpus or canonical WAV/reference"]
async fn profile_recording_dictation() -> anyhow::Result<()> {
    let path = PathBuf::from(std::env::var("SPEAKEASY_FIXTURE_CONFIG")?);
    let config = Config::read(&path)?.validated(&path)?.into_inner();
    let cleanup_mode = std::env::var_os("SPEAKEASY_FIXTURE_CLEANUP");
    let candidate = std::env::var_os("SPEAKEASY_FIXTURE_CANDIDATE").map(PathBuf::from);
    let cleanup = paired::Cleanup::read(cleanup_mode.as_deref(), candidate.as_deref(), &config)?;
    let speed = std::env::var("SPEAKEASY_FIXTURE_SPEED")
        .unwrap_or_else(|_| "1".into())
        .parse::<u32>()?;
    anyhow::ensure!(speed > 0, "Fixture replay speed must be positive");
    let segmenting = std::env::var("SPEAKEASY_FIXTURE_SEGMENT").is_ok_and(|value| value == "1");
    if let Some(path) = std::env::var_os("SPEAKEASY_FIXTURE_CORPUS") {
        let path = PathBuf::from(path);
        let corpus = corpus::Corpus::read(&path)?;
        let mut results = Vec::new();
        for case in &corpus.clips {
            let clip = case.load(path.parent().context("Corpus has no parent directory")?)?;
            let report = recording(&config, clip, speed, segmenting, &cleanup).await?;
            eprintln!(
                "Recording fixture case: {}",
                serde_json::json!({
                    "id": case.id, "tags": case.tags, "report": report,
                })
            );
            results.push((case, report));
        }
        eprintln!(
            "Recording fixture corpus: {}",
            serde_json::json!({
                "corpus_sha256": corpus.sha256,
                "word_scoring": corpus::WORD_SCORING,
                "speed": speed, "segmenting": segmenting,
                "summary": corpus::Summary::new(&results),
            })
        );
    } else {
        let clip = Clip::read(
            &PathBuf::from(std::env::var("SPEAKEASY_FIXTURE_WAV")?),
            &PathBuf::from(std::env::var("SPEAKEASY_FIXTURE_REFERENCE")?),
        )?;
        let report = recording(&config, clip, speed, segmenting, &cleanup).await?;
        eprintln!(
            "Recording fixture profile: {}",
            serde_json::to_string(&report)?
        );
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct Report {
    segmenting: bool,
    continuous: bool,
    speed: u32,
    startup_ms: f64,
    shutdown_ms: f64,
    stop_to_fake_insert_ms: Option<f64>,
    wav_sha256: String,
    reference_sha256: String,
    requests: Vec<Inference>,
    capture: Statistics,
    #[serde(flatten)]
    evaluation: paired::Evaluation,
    total_inference_ms: Option<f64>,
}

impl Report {
    fn scored(
        evaluation: paired::Evaluation,
        stop_to_fake_insert_ms: Option<f64>,
        speed: u32,
    ) -> Self {
        Self {
            segmenting: false,
            continuous: false,
            speed,
            startup_ms: 0.0,
            shutdown_ms: 0.0,
            stop_to_fake_insert_ms,
            wav_sha256: String::new(),
            reference_sha256: String::new(),
            requests: Vec::new(),
            capture: Statistics::default(),
            evaluation,
            total_inference_ms: Some(0.0),
        }
    }
}

async fn recording(
    config: &Config,
    clip: Clip,
    speed: u32,
    segmenting: bool,
    cleanup: &paired::Cleanup,
) -> anyhow::Result<Report> {
    let continuous = segmenting && config.uses_nemotron();
    let wav_sha256 = sha256(&clip.wav);
    let reference_sha256 = sha256(clip.reference.as_bytes());
    let (loaded, ready) = async_channel::bounded(1);
    let (inserted, submitted) = async_channel::bounded(1);
    let (finished, ended) = async_channel::bounded(1);
    let (statistics, drained) = async_channel::bounded(1);
    let (inferences, measured) = async_channel::unbounded();
    let (output, mut updates) = watch::channel(Snapshot::default());
    let (meter, _) = watch::channel(0.0_f32);
    let fixture = Fixture {
        wav: clip.wav,
        loaded,
        inserted,
        replay: Replay {
            speed,
            segmenting,
            finished,
            statistics,
            inferences,
        },
    };
    let started = Instant::now();
    let (recognition_config, style) = cleanup.prepare(config);
    let runtime = Runtime::start_with(
        recognition_config,
        watch::channel(style).1,
        output,
        meter,
        fixture,
    )?;

    let result = async {
        wait_for_signal(&ready, &mut updates, STARTUP_TIMEOUT).await?;
        let startup_ms = milliseconds(started.elapsed());
        runtime.input.deliver(Input::Toggle);
        let stopped = wait_for_signal(&ended, &mut updates, RECORDING_TIMEOUT).await?;
        let phase = timeout(
            REQUEST_TIMEOUT,
            updates.wait_for(|snapshot| {
                matches!(
                    snapshot.phase,
                    Phase::Done | Phase::Empty | Phase::Error | Phase::Cancelled
                )
            }),
        )
        .await??
        .phase;
        if matches!(phase, Phase::Error | Phase::Cancelled) {
            return Err(interruption(&updates.borrow()));
        }
        let (inserted, text) = if phase == Phase::Empty {
            (None, String::new())
        } else {
            let (at, text) = submitted.try_recv()?;
            (Some(at), text)
        };
        let capture = timeout(DRAIN_TIMEOUT, drained.recv()).await??;
        Ok::<_, anyhow::Error>((startup_ms, stopped, inserted, text, capture))
    }
    .await;
    let shutdown = Instant::now();
    runtime.request_stop();
    timeout(SHUTDOWN_TIMEOUT, runtime.stopped()).await?;
    let shutdown_ms = milliseconds(shutdown.elapsed());
    let (startup_ms, stopped, inserted, text, capture) = result?;
    let requests: Vec<_> = std::iter::from_fn(|| measured.try_recv().ok()).collect();
    let total_inference_ms =
        (!continuous).then(|| requests.iter().map(|request| request.elapsed_ms).sum());
    Ok(Report {
        segmenting,
        continuous,
        speed,
        startup_ms,
        shutdown_ms,
        stop_to_fake_insert_ms: inserted
            .map(|at| milliseconds(at.saturating_duration_since(stopped))),
        wav_sha256,
        reference_sha256,
        requests,
        capture,
        evaluation: cleanup.evaluate(&clip.reference, &text, config),
        total_inference_ms,
    })
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
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
