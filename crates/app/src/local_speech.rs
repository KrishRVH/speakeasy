mod streaming;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{fmt, iter, net::TcpListener, path::Path, process::ExitStatus, time::Duration};

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
    runtime::ports::{STARTUP_CANCELLED, Speech},
};

const LOOPBACK: &str = "127.0.0.1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HEALTH_TIMEOUT: Duration = Duration::from_millis(200);
const HEALTH_POLL: Duration = Duration::from_millis(25);

pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);

const INFERENCE_TIMEOUT: Duration = Duration::from_mins(30);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const STOPPED: &str = "Transcription stopped. Check the selected model and engine, then try again.";

const EXIT_GRACE: Duration = Duration::from_millis(500);
const SIGILL: i32 = 4;
const USAGE_ERROR: i32 = 2;
const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
const STATUS_DLL_NOT_FOUND: u32 = 0xC000_0135;
const STATUS_ENTRYPOINT_NOT_FOUND: u32 = 0xC000_0139;
const STATUS_INVALID_IMAGE_FORMAT: u32 = 0xC000_007B;

pub(crate) struct LocalSpeech {
    engine: Engine,
    nemotron: bool,
    group: ProcessGroup,
    child: Child,
    port: u16,
    secret: String,
    client: Client,
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

        drop(listener);
        let mut child = command.spawn().context(
            "Cannot start the speech engine. Check the selected engine executable and its native dependencies.",
        )?;
        let group = contain(&mut child).await?;
        let mut speech = Self {
            engine: config.engine,
            nemotron: config.uses_nemotron(),
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
                    .map_err(|error| warmup.failure(error))
            })
            .await?;
        }
        ensure!(!*cancelled.borrow(), STARTUP_CANCELLED);
        Ok(())
    }

    async fn wait_until_serving(&mut self) -> anyhow::Result<()> {
        let health = self.url(match self.engine {
            Engine::Nemo => "ready",
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
            Engine::Nemo => format!("http://{LOOPBACK}:{port}/{path}"),
            Engine::Whisper => format!("http://{LOOPBACK}:{port}/{secret}/{path}"),
        }
    }

    async fn verify_listener(&mut self) -> anyhow::Result<()> {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return Err(self.kill_and_reap_with_cause().await);
        }
        #[cfg(not(target_os = "macos"))]
        ensure!(
            self.group
                .holds_loopback_listener(self.port)
                .is_ok_and(|held| held),
            STOPPED
        );
        Ok(())
    }

    async fn request<R: Reply>(
        &mut self,
        wav: Vec<u8>,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<R> {
        self.verify_listener().await?;
        let request = match self.engine {
            Engine::Nemo => {
                let rate = audio::wav_sample_rate(&wav)?;
                ensure!(
                    (8_000..=96_000).contains(&rate),
                    "Parakeet needs 8–96 kHz audio. Set your microphone to 48 kHz in your system's sound settings, or choose Whisper."
                );
                let mut form = recording_form(wav, R::FORMAT)?;
                if self.nemotron {
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
                    .bearer_auth(&self.secret)
                    .multipart(form)
            },
            Engine::Whisper => {
                let form = recording_form(wav, R::FORMAT)?
                    .text(
                        "language",
                        crate::models::whisper_language(language).to_owned(),
                    )
                    .text("temperature", "0.0");
                self.client.post(self.url("inference")).multipart(form)
            },
        };
        match exchange(request).await {
            Ok(body) => reply(&body),
            Err(Unanswered::Disconnected) => Err(self.kill_and_reap_with_cause().await),
            Err(Unanswered::Failed(error)) => Err(error),
        }
    }

    async fn kill_and_reap_with_cause(&mut self) -> anyhow::Error {
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
        audio: crate::runtime::ports::LiveAudio,
        language: &str,
        terms: &Terms,
    ) -> anyhow::Result<Option<String>> {
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
        self.transcribe(
            silence,
            if self.nemotron { "en-US" } else { "en" },
            &Terms::default(),
        )
        .await?;
        Ok(())
    }

    async fn stop(&mut self) {
        self.group.terminate();
        kill_and_reap(&mut self.child).await;
    }
}

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

fn reply<T: DeserializeOwned>(body: &[u8]) -> anyhow::Result<T> {
    serde_json::from_slice(body)
        .ok()
        .context("The speech engine sent a reply Speakeasy cannot read. Check the selected model and engine, then try again.")
}

trait Reply: DeserializeOwned {
    const FORMAT: &'static str;
}

#[derive(Deserialize)]
struct Plain {
    text: Option<String>,
}

impl Reply for Plain {
    const FORMAT: &'static str = "json";
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
}

#[derive(Deserialize)]
struct TimedSegment {
    #[serde(default)]
    words: Vec<Token>,
}

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
    fn into_narration(self) -> Narration {
        let heard: Vec<Heard> = if self.words.is_empty() {
            joined(self.segments.into_iter().flat_map(|segment| segment.words))
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

fn joined(tokens: impl Iterator<Item = Token>) -> Vec<Heard> {
    let mut words: Vec<Heard> = Vec::new();
    for token in tokens {
        if token.word.trim().is_empty() || token.word.trim_start().starts_with("[_") {
            continue;
        }
        match words.last_mut() {
            Some(word) if !token.word.starts_with(char::is_whitespace) => {
                word.span = word
                    .span
                    .zip(token.span())
                    .map(|(whole, piece)| whole.union(piece));
                word.text.push_str(&token.word);
            },
            _ => words.push(token.into_heard()),
        }
    }
    words
}

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

fn random_token() -> anyhow::Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(format!("{:032x}", u128::from_le_bytes(bytes)))
}

fn server_command(config: &Config, port: u16, secret: &str, served: &Path) -> Command {
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
                .env("NEMO_SPEECH_HTTP_API_KEY", secret);
        },
        Engine::Whisper => {
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
                    crate::models::whisper_language(&config.language),
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

async fn exchange(request: RequestBuilder) -> Result<Vec<u8>, Unanswered> {
    let mut response = request.timeout(INFERENCE_TIMEOUT).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(Unanswered::Failed(anyhow!(
            "The speech engine returned HTTP {}. Check the selected model and engine, then try again.",
            status.as_u16()
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(Unanswered::Failed(anyhow!(
                "The speech engine sent a reply too large to read. Check the selected model and engine, then try again."
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
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
