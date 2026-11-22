mod streaming;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{fmt, iter, net::TcpListener, ops::RangeInclusive, process::ExitStatus, time::Duration};

use anyhow::{Context as _, anyhow, bail, ensure};
use reqwest::{Client, RequestBuilder, multipart};
use serde::{Deserialize, de::DeserializeOwned};
use speakeasy_platform::ProcessGroup;
use speakeasy_show::{Moment, Narration, Span, Word};
use tokio::{
    process::{Child, Command},
    sync::watch,
    time::{sleep, timeout},
};

use crate::{
    audio,
    child::{hidden_command, kill_and_reap},
    config::{Config, Engine},
    http::{Trust, client_builder},
    recognition_terms::Terms,
    runtime::ports::{LiveAudio, STARTUP_CANCELLED, Speech},
};

const LOOPBACK: &str = "127.0.0.1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HEALTH_TIMEOUT: Duration = Duration::from_millis(200);
const HEALTH_POLL: Duration = Duration::from_millis(25);

/// Bounds each startup stage on its own: serving, then any warmup.
pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

/// Generous on purpose: CPU inference of a five-minute recording can take minutes.
///
/// Escape and bounded recovery end real waits long before this.
const INFERENCE_TIMEOUT: Duration = Duration::from_mins(30);
const PARAKEET_RATES: RangeInclusive<u32> = 8_000..=96_000;
const STOPPED: &str = "Transcription stopped. Check the selected model and engine, then try again.";

/// How long a killed engine's exit status may take to become reapable.
const EXIT_GRACE: Duration = Duration::from_millis(500);
const SIGILL: i32 = 4;
const USAGE_ERROR: i32 = 2;
const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
const STATUS_DLL_NOT_FOUND: u32 = 0xC000_0135;
const STATUS_ENTRYPOINT_NOT_FOUND: u32 = 0xC000_0139;
const STATUS_INVALID_IMAGE_FORMAT: u32 = 0xC000_007B;

pub(crate) struct LocalSpeech {
    protocol: Protocol,
    /// Declared before `child`, so dropping kills the group before its leader can be reaped.
    group: ProcessGroup,
    child: Child,
    port: u16,
    client: Client,
}

impl LocalSpeech {
    pub(crate) async fn start(
        config: Config,
        mut cancelled: watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        ensure!(!*cancelled.borrow(), STARTUP_CANCELLED);
        let port = TcpListener::bind((LOOPBACK, 0))?.local_addr()?.port();
        let client = engine_client()?;
        let mut child = server_command(&config, port).spawn().context(
            "Cannot start the speech engine. Check the selected engine executable and its native dependencies.",
        )?;
        let group = contain(&mut child).await?;
        let protocol = match config.engine {
            Engine::Whisper => Protocol::Whisper,
            Engine::Nemo if config.uses_nemotron() => Protocol::Nemotron,
            Engine::Nemo => Protocol::Nemo,
        };
        let mut speech = Self {
            protocol,
            group,
            child,
            port,
            client,
        };
        if let Err(error) = speech.become_ready(&config, &mut cancelled).await {
            speech.stop().await;
            return Err(error);
        }
        Ok(speech)
    }

    async fn become_ready(
        &mut self,
        config: &Config,
        cancelled: &mut watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        unless_cancelled(cancelled, async {
            timeout(STARTUP_TIMEOUT, self.wait_until_serving())
                .await
                .context(
                    "Loading the model timed out. Check the selected model and engine, then try again.",
                )?
        })
        .await?;
        if let Some(warmup) = Warmup::required(config) {
            unless_cancelled(cancelled, async {
                timeout(STARTUP_TIMEOUT, self.probe_with_silence())
                    .await
                    .context(warmup.timed_out)?
                    .map_err(|error| warmup.failure(error))
            })
            .await?;
        }
        ensure!(!*cancelled.borrow(), STARTUP_CANCELLED);
        Ok(())
    }

    async fn wait_until_serving(&mut self) -> anyhow::Result<()> {
        let health = self.url(match self.protocol {
            Protocol::Whisper => "health",
            Protocol::Nemo | Protocol::Nemotron => "ready",
        });
        loop {
            if self.leader_exited()? {
                self.group.terminate();
                bail!(startup_exit(self.child.wait().await?));
            }
            let probe = self.client.get(&health).timeout(HEALTH_TIMEOUT).send();
            if probe
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(());
            }
            sleep(HEALTH_POLL).await;
        }
    }

    #[cfg(unix)]
    fn leader_exited(&self) -> std::io::Result<bool> {
        self.group.leader_exited()
    }

    #[cfg(target_os = "windows")]
    fn leader_exited(&mut self) -> std::io::Result<bool> {
        self.child.try_wait().map(|status| status.is_some())
    }

    fn url(&self, endpoint: &str) -> String {
        format!("http://{LOOPBACK}:{}/{endpoint}", self.port)
    }

    async fn request<R: Reply>(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<R> {
        if !matches!(self.leader_exited(), Ok(false)) {
            return Err(self.terminate_with_cause().await);
        }
        let request = match self.protocol {
            Protocol::Whisper => {
                let mut form = recording_form(wav, R::FORMAT)?
                    .text(
                        "language",
                        crate::models::whisper_language(language).to_owned(),
                    )
                    .text("temperature", "0.0");
                if R::WORDS {
                    form = form.text("split_on_word", "true");
                }
                if !terms.phrases().is_empty() {
                    form = form.text("prompt", terms.prompt());
                }
                self.client.post(self.url("inference")).multipart(form)
            },
            Protocol::Nemo | Protocol::Nemotron => {
                let rate = audio::wav_sample_rate(&wav)?;
                ensure!(
                    PARAKEET_RATES.contains(&rate),
                    "Parakeet needs 8–96 kHz audio. Set your microphone to 48 kHz in your system's sound settings, or choose Whisper."
                );
                let mut form = recording_form(wav, R::FORMAT)?;
                if self.protocol == Protocol::Nemotron {
                    form = form.text(
                        "language",
                        crate::models::nemotron_language(language).to_owned(),
                    );
                    if !terms.phrases().is_empty() {
                        form = form.text("speech_contexts", terms.contexts().to_string());
                    }
                }
                self.client
                    .post(self.url("v1/audio/transcriptions"))
                    .multipart(form)
            },
        };
        match exchange(request).await {
            Ok(body) => reply(&body),
            Err(Unanswered::Disconnected) => Err(self.terminate_with_cause().await),
            Err(Unanswered::Failed(error)) => Err(error),
        }
    }

    /// Kills the engine's group, then reads its exit status for an exact remedy.
    ///
    /// On Unix the group can be signaled only while its leader is unreaped, so the kill comes
    /// first. No cause is lost: a crash fixes its exit status before its connections close.
    async fn terminate_with_cause(&mut self) -> anyhow::Error {
        self.group.terminate();
        if let Ok(Ok(status)) = timeout(EXIT_GRACE, self.child.wait()).await
            && let Some(remedy) = exit_remedy(status)
        {
            return Incompatible { status, remedy }.into();
        }
        anyhow!(STOPPED)
    }
}

impl Speech for LocalSpeech {
    async fn transcribe_stream(
        &mut self,
        audio: LiveAudio,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<Option<String>> {
        // Continuous recognition is optional, so a stream failure yields `None`, not an error.
        // The probe then confirms the engine is alive and has dropped the abandoned stream job.
        let text = streaming::transcribe(self, audio, language, terms)
            .await
            .unwrap_or(None);
        if text.is_none() {
            timeout(CONNECT_TIMEOUT, self.probe_with_silence())
                .await
                .context(STOPPED)??;
        }
        Ok(text)
    }

    async fn transcribe(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<String> {
        self.request::<Plain>(wav, language, terms)
            .await?
            .text
            .context("The speech engine returned no transcript. Check the selected model and engine, then try again.")
    }

    async fn transcribe_words(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<Narration> {
        self.request::<Timed>(wav, language, terms)
            .await
            .map(Timed::into_narration)
    }

    async fn probe_with_silence(&mut self) -> anyhow::Result<()> {
        let silence = audio::silent_wav(16_000, Duration::from_secs(1))?;
        self.transcribe(silence, "en", &Terms::default()).await?;
        Ok(())
    }

    async fn stop(&mut self) {
        self.group.terminate();
        kill_and_reap(&mut self.child).await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Whisper,
    Nemo,
    Nemotron,
}

/// Configurations that must transcribe silence before reporting ready.
///
/// Loading weights neither initializes every GPU kernel nor proves that a Linux build can run
/// its CPU kernels.
struct Warmup {
    timed_out: &'static str,
    failed: &'static str,
}

impl Warmup {
    fn required(config: &Config) -> Option<Self> {
        if config.use_gpu {
            Some(Self {
                timed_out: "GPU warmup timed out. Check the selected engine and its GPU dependencies.",
                failed: "GPU warmup failed. Check the selected engine and its GPU dependencies.",
            })
        } else if cfg!(target_os = "linux") {
            Some(Self {
                timed_out: "CPU transcription timed out. Choose a smaller model or a compatible engine executable in Settings.",
                failed: "CPU transcription failed. Check the selected model, or choose a compatible engine executable in Settings.",
            })
        } else {
            None
        }
    }

    fn failure(&self, error: anyhow::Error) -> anyhow::Error {
        if error.is::<Incompatible>() {
            error
        } else {
            error.context(self.failed)
        }
    }
}

/// Drops serde's error: it quotes unexpected strings, which can be transcript text.
fn reply<T: DeserializeOwned>(body: &[u8]) -> anyhow::Result<T> {
    serde_json::from_slice(body)
        .ok()
        .context("The speech engine sent a reply Speakeasy cannot read. Check the selected model and engine, then try again.")
}

trait Reply: DeserializeOwned {
    const FORMAT: &'static str;
    const WORDS: bool;
}

#[derive(Deserialize)]
struct Plain {
    text: Option<String>,
}

impl Reply for Plain {
    const FORMAT: &'static str = "json";
    const WORDS: bool = false;
}

#[derive(Deserialize)]
struct Timed {
    #[serde(default)]
    text: String,
    #[serde(default)]
    words: Vec<Token>,
    #[serde(default)]
    segments: Vec<TimedSegment>,
}

impl Reply for Timed {
    const FORMAT: &'static str = "verbose_json";
    const WORDS: bool = true;
}

#[derive(Deserialize)]
struct TimedSegment {
    #[serde(default)]
    words: Vec<Token>,
}

/// A word or word piece, timed in seconds from the start of the audio.
///
/// A time the engine could not compute is absent or `null`, as JSON writers print `NaN`.
#[derive(Deserialize)]
struct Token {
    word: String,
    start: Option<f64>,
    end: Option<f64>,
}

struct Heard {
    text: String,
    span: Option<Span>,
}

impl Timed {
    /// Times every word, or falls back to untimed text when any time is missing or invalid.
    ///
    /// A guessed time would silently misplace a word.
    fn into_narration(self) -> Narration {
        let heard: Vec<Heard> = if self.words.is_empty() {
            self.segments
                .into_iter()
                .flat_map(|segment| joined(segment.words))
                .collect()
        } else {
            self.words
                .into_iter()
                .filter(|token| !token.word.trim().is_empty())
                .map(Token::into_heard)
                .collect()
        };
        let text = self.text.trim();
        let spans: Option<Vec<Span>> = heard.iter().map(|word| word.span).collect();
        match spans {
            Some(spans) if !spans.is_empty() || text.is_empty() => Narration::Words(
                iter::zip(spans, heard)
                    .map(|(span, Heard { text, .. })| Word { span, text })
                    .collect(),
            ),
            _ if !text.is_empty() => Narration::Text(text.to_owned()),
            _ => Narration::Text(
                heard
                    .into_iter()
                    .map(|word| word.text)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        }
    }
}

impl Token {
    fn into_heard(self) -> Heard {
        Heard {
            span: self.span(),
            text: self.word.trim().to_owned(),
        }
    }

    fn span(&self) -> Option<Span> {
        let time =
            |seconds: Option<f64>| Duration::try_from_secs_f64(seconds?).ok().map(Moment::from);
        let (start, end) = (time(self.start)?, time(self.end)?);
        (start <= end).then(|| Span::new(start, end))
    }
}

/// Rejoins Whisper's word-piece tokens; a token that starts with a space starts a word.
///
/// Its `[_`-prefixed control tokens are not speech.
fn joined(tokens: Vec<Token>) -> Vec<Heard> {
    let mut words: Vec<Heard> = Vec::new();
    let mut boundary = true;
    for token in tokens {
        let text = token.word.trim();
        if text.is_empty() {
            boundary = true;
            continue;
        }
        if text.starts_with("[_") {
            continue;
        }
        let ends_word = token.word.ends_with(char::is_whitespace);
        match words.last_mut() {
            Some(word) if !boundary && !token.word.starts_with(char::is_whitespace) => {
                word.span = word
                    .span
                    .zip(token.span())
                    .map(|(whole, piece)| whole.union(piece));
                word.text.push_str(text);
            },
            _ => words.push(token.into_heard()),
        }
        boundary = ends_word;
    }
    words
}

fn server_command(config: &Config, port: u16) -> Command {
    let port = port.to_string();
    let mut command = hidden_command(&config.engine_executable);
    match config.engine {
        Engine::Nemo => {
            let device = if config.use_gpu { "auto" } else { "cpu" };
            command
                .args(["serve", "--asr-model"])
                .arg(&config.model)
                .args([
                    "--device",
                    device,
                    "--host",
                    LOOPBACK,
                    "--port",
                    &port,
                    "--no-ui",
                    "--threads",
                    "1",
                    "--asr.batching.enabled=false",
                ])
                // An inherited key would make every request fail with 401 after `/ready` succeeds.
                .env_remove("NEMO_SPEECH_HTTP_API_KEY");
        },
        Engine::Whisper => {
            command.arg("--model").arg(&config.model).args([
                "--host",
                LOOPBACK,
                "--port",
                &port,
                "--threads",
                &config.threads.to_string(),
                "--language",
                crate::models::whisper_language(&config.language),
            ]);
            if !config.use_gpu {
                command.arg("--no-gpu");
            }
        },
    }
    // ggml turns its Metal tensor kernels on by itself for some newer GPUs, where they return no
    // speech or garbage text.
    #[cfg(target_os = "macos")]
    command.env("GGML_METAL_TENSOR_DISABLE", "1");
    #[cfg(unix)]
    command.process_group(0);
    command
}

fn engine_client() -> anyhow::Result<Client> {
    Ok(client_builder(Trust::Nobody)?
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_max_idle_per_host(0)
        .build()?)
}

async fn contain(child: &mut Child) -> anyhow::Result<ProcessGroup> {
    match ProcessGroup::attach(child) {
        Ok(group) => Ok(group),
        Err(error) => {
            kill_and_reap(child).await;
            Err(anyhow!(
                "Cannot start the speech engine under Speakeasy's control ({error:#}). Restart Speakeasy, then try again."
            ))
        },
    }
}

async fn unless_cancelled<T>(
    cancelled: &mut watch::Receiver<bool>,
    work: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::select! {
        biased;
        _ = cancelled.changed() => bail!(STARTUP_CANCELLED),
        result = work => result,
    }
}

fn recording_form(wav: Vec<u8>, format: &'static str) -> reqwest::Result<multipart::Form> {
    let file = multipart::Part::bytes(wav)
        .file_name("dictation.wav")
        .mime_str("audio/wav")?;
    Ok(multipart::Form::new()
        .part("file", file)
        .text("response_format", format))
}

async fn exchange(request: RequestBuilder) -> Result<Vec<u8>, Unanswered> {
    let response = request.timeout(INFERENCE_TIMEOUT).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(Unanswered::Failed(anyhow!(
            "The speech engine returned HTTP {}. Check the selected model and engine, then try again.",
            status.as_u16()
        )));
    }
    Ok(response.bytes().await?.to_vec())
}

enum Unanswered {
    Disconnected,
    Failed(anyhow::Error),
}

impl From<reqwest::Error> for Unanswered {
    fn from(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Failed(anyhow!(
                "Transcription timed out. Try a smaller model, or turn on Prefer GPU in Settings."
            ))
        } else {
            Self::Disconnected
        }
    }
}

fn startup_exit(status: ExitStatus) -> String {
    let remedy = if status.code() == Some(USAGE_ERROR) {
        "The selected engine does not accept Speakeasy's server arguments. Switch Engine to match it, or choose a supported engine executable in Settings."
    } else {
        exit_remedy(status).unwrap_or(
            "Check the selected model and the engine's dependencies, or choose a compatible engine executable in Settings.",
        )
    };
    format!("The speech engine exited before it was ready ({status}). {remedy}")
}

#[derive(Debug)]
struct Incompatible {
    status: ExitStatus,
    remedy: &'static str,
}

impl fmt::Display for Incompatible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { status, remedy } = self;
        write!(f, "The speech engine exited ({status}). {remedy}")
    }
}

impl std::error::Error for Incompatible {}

fn exit_remedy(status: ExitStatus) -> Option<&'static str> {
    #[cfg(unix)]
    let signal = status.signal();
    #[cfg(not(unix))]
    let signal = None;
    match (status.code().map(i32::cast_unsigned), signal) {
        (Some(STATUS_ILLEGAL_INSTRUCTION), _) | (_, Some(SIGILL)) => Some(
            "This engine uses CPU instructions unavailable on this machine. Choose a compatible engine executable in Settings.",
        ),
        (
            Some(STATUS_DLL_NOT_FOUND | STATUS_ENTRYPOINT_NOT_FOUND | STATUS_INVALID_IMAGE_FORMAT),
            _,
        ) => Some(
            "This engine needs native libraries that are missing or incompatible. Install its dependencies, or choose another engine executable in Settings.",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
