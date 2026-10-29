//! One local speech engine process behind a loopback HTTP API. The process is contained and reaped
//! on every exit path, and its output never reaches diagnostics.

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{net::TcpListener, path::Path, process::ExitStatus, time::Duration};

use anyhow::{Context as _, anyhow, bail, ensure};
use reqwest::{Client, Response, multipart};
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
    runtime::ports::{STARTUP_CANCELLED, Speech},
};

const LOOPBACK: &str = "127.0.0.1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HEALTH_TIMEOUT: Duration = Duration::from_millis(200);
const HEALTH_POLL: Duration = Duration::from_millis(25);
/// Bounds each startup stage on its own: serving, then any warm-up.
pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
// CPU inference of a five-minute recording can take minutes; Escape and bounded recovery end real
// waits long before this.
const INFERENCE_TIMEOUT: Duration = Duration::from_mins(30);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const STOPPED: &str = "Transcription stopped. Check the selected model and engine, then try again.";
/// The engine returned word times Speakeasy cannot place on the recording.
const INVALID_TIMINGS: &str = "The speech engine returned invalid word timings. Check the selected model and engine, then try again.";
const SIGILL: i32 = 4;
const USAGE_ERROR: u32 = 2;
const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
const STATUS_DLL_NOT_FOUND: u32 = 0xC000_0135;
const STATUS_ENTRYPOINT_NOT_FOUND: u32 = 0xC000_0139;
const STATUS_INVALID_IMAGE_FORMAT: u32 = 0xC000_007B;

pub(crate) struct LocalSpeech {
    engine: Engine,
    /// Declared before `child`, so dropping terminates the group before its leader can be reaped.
    group: ProcessGroup,
    child: Child,
    port: u16,
    /// Parakeet's API key, passed only through its environment, and Whisper's route, which is on
    /// its argv and so visible to other local users.
    secret: String,
    client: Client,
    /// Whisper serves this empty directory instead of its bundled web UI.
    _directory: tempfile::TempDir,
}

impl LocalSpeech {
    pub(crate) async fn start(
        config: Config,
        mut cancelled: watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        ensure!(!*cancelled.borrow(), STARTUP_CANCELLED);
        let listener = TcpListener::bind((LOOPBACK, 0))?;
        let port = listener.local_addr()?.port();
        let directory = served_directory()?;
        let secret = random_token()?;
        let mut command = server_command(&config, port, &secret, directory.path());
        let client = engine_client()?;
        // Release the reserved port just before the engine binds it.
        drop(listener);
        let mut child = command.spawn().context(
            "Cannot start the speech engine. Check the selected engine executable and its native dependencies.",
        )?;
        let group = contain(&mut child).await?;
        let mut speech = Self {
            engine: config.engine,
            group,
            child,
            port,
            secret,
            client,
            _directory: directory,
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
                    .context(warmup.failed)
            })
            .await?;
        }
        ensure!(!*cancelled.borrow(), STARTUP_CANCELLED);
        Ok(())
    }

    async fn wait_until_serving(&mut self) -> anyhow::Result<()> {
        let health = self.url(match self.engine {
            Engine::Parakeet => "ready",
            Engine::Whisper => "health",
        });
        loop {
            if let Some(status) = self.child.try_wait()? {
                bail!(startup_exit(status));
            }
            if let Ok(response) = self
                .client
                .get(&health)
                .timeout(HEALTH_TIMEOUT)
                .send()
                .await
                && response.status().is_success()
            {
                // The reserved port is released before the engine binds it, so another program can
                // take it and answer. That program must never receive audio or choose the text to
                // insert. Whisper's probe carries its route secret, which opens nothing on an
                // engine that lost the port. macOS has no ownership lookup, so it trusts the answer
                // and each request checks that the engine, which exits once its port is lost,
                // still runs.
                #[cfg(not(target_os = "macos"))]
                ensure!(
                    self.group
                        .holds_loopback_listener(self.port)
                        .context("Cannot verify the speech engine's process. Try again.")?,
                    "Another program took the speech engine's port. Try again."
                );
                return Ok(());
            }
            sleep(HEALTH_POLL).await;
        }
    }

    fn url(&self, path: &str) -> String {
        let Self {
            engine,
            port,
            secret,
            ..
        } = self;
        match engine {
            Engine::Parakeet => format!("http://{LOOPBACK}:{port}/{path}"),
            Engine::Whisper => format!("http://{LOOPBACK}:{port}/{secret}/{path}"),
        }
    }

    /// Sends `wav` for transcription and reads the engine's reply as `R`.
    async fn request<R: Reply>(&mut self, wav: Vec<u8>, language: &str) -> anyhow::Result<R> {
        // The engine may have exited and lost its port since readiness. Each request connects
        // afresh, leaving only the moment between these checks and that connection.
        ensure!(matches!(self.child.try_wait(), Ok(None)), STOPPED);
        #[cfg(not(target_os = "macos"))]
        ensure!(
            self.group
                .holds_loopback_listener(self.port)
                .is_ok_and(|held| held),
            STOPPED
        );
        let request = match self.engine {
            Engine::Parakeet => {
                let rate = audio::wav_sample_rate(&wav)?;
                ensure!(
                    (8_000..=96_000).contains(&rate),
                    "Parakeet needs 8–96 kHz audio. Set your microphone to 48 kHz in your system's sound settings, or choose Whisper."
                );
                self.client
                    .post(self.url("v1/audio/transcriptions"))
                    .bearer_auth(&self.secret)
                    .multipart(recording_form(wav, R::FORMAT)?)
            },
            Engine::Whisper => {
                let form = recording_form(wav, R::FORMAT)?
                    .text("language", language.to_owned())
                    .text("temperature", "0.0");
                self.client.post(self.url("inference")).multipart(form)
            },
        };
        let response = request
            .timeout(INFERENCE_TIMEOUT)
            .send()
            .await
            .map_err(transport_error)?;
        ensure!(
            response.status().is_success(),
            "The speech engine returned HTTP {}. Check the selected model and engine, then try again.",
            response.status().as_u16()
        );
        reply(&read_bounded(response).await?)
    }
}

impl Speech for LocalSpeech {
    async fn transcribe(&mut self, wav: Vec<u8>, language: &str) -> anyhow::Result<String> {
        self.request::<Plain>(wav, language)
            .await?
            .text
            .context("The speech engine returned no transcript. Check the selected model and engine, then try again.")
    }

    async fn transcribe_words(
        &mut self,
        wav: Vec<u8>,
        language: &str,
    ) -> anyhow::Result<Narration> {
        self.request::<Timed>(wav, language).await?.into_narration()
    }

    async fn probe_with_silence(&mut self) -> anyhow::Result<()> {
        let silence = audio::silent_wav(16_000, Duration::from_secs(1))?;
        self.transcribe(silence, "en").await?;
        Ok(())
    }

    async fn stop(&mut self) {
        self.group.terminate();
        kill_and_reap(&mut self.child).await;
    }
}

/// Loading weights neither initializes every GPU kernel nor proves that a Linux build can run its
/// CPU kernels, so these configurations transcribe silence before reporting ready.
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
}

/// Parses an engine reply. Serde quotes strings it did not expect, and those can be transcript
/// text, so its error is never kept as a source.
fn reply<T: DeserializeOwned>(body: &[u8]) -> anyhow::Result<T> {
    serde_json::from_slice(body)
        .ok()
        .context("The speech engine sent a reply Speakeasy cannot read. Check the selected model and engine, then try again.")
}

/// An engine reply Speakeasy reads, and the `response_format` that asks for it.
trait Reply: DeserializeOwned {
    const FORMAT: &'static str;
}

/// The part of either engine's JSON reply that Speakeasy reads.
#[derive(Deserialize)]
struct Plain {
    text: Option<String>,
}

impl Reply for Plain {
    const FORMAT: &'static str = "json";
}

/// The part of either engine's timed reply that Speakeasy reads. Parakeet lists whole words;
/// whisper.cpp lists tokens within segments.
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
}

#[derive(Deserialize)]
struct TimedSegment {
    #[serde(default)]
    words: Vec<Token>,
}

/// A word or word piece, timed in seconds from the start of the audio.
#[derive(Deserialize)]
struct Token {
    word: String,
    start: f64,
    end: f64,
}

impl Timed {
    fn into_narration(self) -> anyhow::Result<Narration> {
        let words = if self.words.is_empty() {
            joined(self.segments.into_iter().flat_map(|segment| segment.words))?
        } else {
            self.words
                .into_iter()
                .filter(|token| !token.word.trim().is_empty())
                .map(Token::into_word)
                .collect::<anyhow::Result<_>>()?
        };
        let text = self.text.trim();
        Ok(if words.is_empty() && !text.is_empty() {
            Narration::Text(text.to_owned())
        } else {
            Narration::Words(words)
        })
    }
}

impl Token {
    fn into_word(self) -> anyhow::Result<Word> {
        Ok(Word {
            span: self.span()?,
            text: self.word.trim().to_owned(),
        })
    }

    fn span(&self) -> anyhow::Result<Span> {
        let time = |seconds: f64| {
            Duration::try_from_secs_f64(seconds)
                .map(Moment::from)
                .ok()
                .context(INVALID_TIMINGS)
        };
        let (start, end) = (time(self.start)?, time(self.end)?);
        ensure!(start <= end, INVALID_TIMINGS);
        Ok(Span::new(start, end))
    }
}

/// whisper.cpp splits words into tokens; a token that starts with a space starts a word. Its
/// bracketed control tokens are not speech.
fn joined(tokens: impl Iterator<Item = Token>) -> anyhow::Result<Vec<Word>> {
    let mut words: Vec<Word> = Vec::new();
    for token in tokens {
        if token.word.trim().is_empty() || token.word.trim_start().starts_with("[_") {
            continue;
        }
        match words.last_mut() {
            Some(word) if !token.word.starts_with(char::is_whitespace) => {
                word.span = word.span.union(token.span()?);
                word.text.push_str(&token.word);
            },
            _ => words.push(token.into_word()?),
        }
    }
    Ok(words)
}

/// The empty directory Whisper serves in place of its bundled web UI. tempfile's error already
/// names the folder it tried, escaped, so the message rebuilds the bare OS reason and names the
/// folder once.
fn served_directory() -> anyhow::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("speakeasy-")
        .tempdir()
        .map_err(|error| {
            let reason = error.raw_os_error().map_or_else(
                || error.kind().to_string(),
                |code| std::io::Error::from_raw_os_error(code).to_string(),
            );
            anyhow!(
                "Cannot use the temporary folder {}: {reason}. {}",
                std::env::temp_dir().display(),
                crate::paths::remedy(&error)
            )
        })
}

/// 128 bits from the operating system's generator as 32 hex digits. A shared temporary directory
/// lists its entries' names to every local user, so no name can be the secret.
fn random_token() -> anyhow::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(format!("{:032x}", u128::from_le_bytes(bytes)))
}

fn server_command(config: &Config, port: u16, secret: &str, served: &Path) -> Command {
    let port = port.to_string();
    let mut command = hidden_command(&config.engine_executable);
    match config.engine {
        Engine::Parakeet => {
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
                .env("NEMO_SPEECH_HTTP_API_KEY", secret);
        },
        Engine::Whisper => {
            // Keep default timestamp decoding: disabling it can skip speech when a window ends
            // early, and the JSON text stays plain either way.
            command
                .arg("--model")
                .arg(&config.model)
                .args([
                    "--host",
                    LOOPBACK,
                    "--port",
                    &port,
                    "--request-path",
                    &format!("/{secret}"),
                    "--public",
                ])
                .arg(served)
                .args([
                    "--threads",
                    &config.threads.to_string(),
                    "--language",
                    &config.language,
                ]);
            if !config.use_gpu {
                command.arg("--no-gpu");
            }
        },
    }
    #[cfg(unix)]
    command.process_group(0);
    command
}

/// A client that reaches only the engine: never through a proxy, never after a redirect, and
/// never over an idle pooled connection, which could outlive the listener check that precedes
/// each request.
fn engine_client() -> anyhow::Result<Client> {
    Ok(client_builder(Trust::Nobody)?
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
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

/// A failed exchange with the engine as a fixed message. The request URL carries the private route,
/// so the transport error never reaches users.
fn transport_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow!("Transcription timed out. Try a smaller model, or turn on Prefer GPU in Settings.")
    } else {
        anyhow!(STOPPED)
    }
}

async fn read_bounded(mut response: Response) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        ensure!(
            chunk.len() <= MAX_RESPONSE_BYTES.saturating_sub(body.len()),
            "The speech engine sent a reply too large to read. Check the selected model and engine, then try again."
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn startup_exit(status: ExitStatus) -> String {
    #[cfg(unix)]
    let signal = status.signal();
    #[cfg(not(unix))]
    let signal = None;
    format!(
        "The speech engine exited before it was ready ({status}). {}",
        exit_remedy(status.code(), signal)
    )
}

fn exit_remedy(code: Option<i32>, signal: Option<i32>) -> &'static str {
    match (code.map(i32::cast_unsigned), signal) {
        (Some(STATUS_ILLEGAL_INSTRUCTION), _) | (_, Some(SIGILL)) => {
            "This engine uses CPU instructions unavailable on this machine. Choose a compatible engine executable in Settings."
        },
        (
            Some(STATUS_DLL_NOT_FOUND | STATUS_ENTRYPOINT_NOT_FOUND | STATUS_INVALID_IMAGE_FORMAT),
            _,
        ) => {
            "This engine needs native libraries that are missing or incompatible. Install its dependencies, or choose another engine executable in Settings."
        },
        (Some(USAGE_ERROR), _) => {
            "The selected engine does not accept Speakeasy's server arguments. Switch Engine to match it, or choose a supported engine executable in Settings."
        },
        _ => {
            "Check the selected model and the engine's dependencies, or choose a compatible engine executable in Settings."
        },
    }
}

#[cfg(test)]
mod tests;
