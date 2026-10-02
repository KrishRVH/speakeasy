//! An engine's own server process behind a loopback HTTP API: Whisper, or a Parakeet installation
//! without the engine's C library.

use std::{
    net::TcpListener,
    os::unix::process::ExitStatusExt,
    path::Path,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, anyhow, bail, ensure};
use reqwest::{Client, Response, multipart};
use serde::Deserialize;
use speakeasy_platform::ProcessGroup;
use tokio::{
    process::{Child, Command},
    sync::watch,
    time::{sleep, timeout},
};

use super::{
    INFERENCE_TIMEOUT, MAX_TEXT_BYTES, STARTUP_TIMEOUT, Warmup, contain, unless_cancelled,
};
use crate::{
    audio,
    child::{kill_and_reap, owned_command},
    config::{Config, Engine},
    ports::{STARTUP_CANCELLED, Speech},
};

const LOOPBACK: &str = "127.0.0.1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HEALTH_TIMEOUT: Duration = Duration::from_millis(200);
const HEALTH_POLL: Duration = Duration::from_millis(25);
const SIGILL: i32 = 4;
const USAGE_ERROR: u32 = 2;

pub(crate) struct Server {
    engine: Engine,
    /// Declared before `child`, so dropping terminates the group before its leader can be reaped.
    group: ProcessGroup,
    child: Child,
    endpoint: String,
    /// The private Whisper route and the `NeMo` API key.
    secret: String,
    client: Client,
    /// Whisper serves this empty directory; its random name is `secret`.
    _directory: tempfile::TempDir,
}

impl Server {
    pub(crate) async fn start(
        config: Config,
        mut cancelled: watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        ensure!(!*cancelled.borrow(), STARTUP_CANCELLED);
        let listener = TcpListener::bind((LOOPBACK, 0))?;
        let port = listener.local_addr()?.port();
        let directory = tempfile::Builder::new().prefix("speakeasy-").tempdir()?;
        let secret = directory
            .path()
            .file_name()
            .context("Missing temporary directory name")?
            .to_string_lossy()
            .into_owned();
        let mut command = server_command(&config, port, &secret, directory.path());
        // Release the reserved port just before the engine binds it.
        drop(listener);
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        let mut child = command.spawn().context(
            "Cannot start local speech. Check the selected engine executable and its native dependencies.",
        )?;
        let group = contain(&mut child).await?;
        let endpoint = match config.engine {
            Engine::Parakeet => format!("http://{LOOPBACK}:{port}"),
            Engine::Whisper => format!("http://{LOOPBACK}:{port}/{secret}"),
        };
        let mut speech = Self {
            engine: config.engine,
            group,
            child,
            endpoint,
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
                .context("Local model startup timed out")?
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
                // The reaped leader's process ID may be reused, so its group is never signaled.
                self.group.disarm();
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
                return Ok(());
            }
            sleep(HEALTH_POLL).await;
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.endpoint)
    }
}

impl Speech for Server {
    async fn transcribe(&self, wav: Vec<u8>, language: &str) -> anyhow::Result<String> {
        let request = match self.engine {
            Engine::Parakeet => {
                let rate = audio::wav_sample_rate(&wav).context("Missing recording sample rate")?;
                ensure!(
                    (8_000..=96_000).contains(&rate),
                    "Parakeet needs 8–96 kHz audio. Set your microphone to 48 kHz in OS settings or choose Whisper."
                );
                self.client
                    .post(self.url("v1/audio/transcriptions"))
                    .bearer_auth(&self.secret)
                    .multipart(recording_form(wav)?)
            },
            Engine::Whisper => {
                let form = recording_form(wav)?
                    .text("language", language.to_owned())
                    .text("temperature", "0.0");
                self.client.post(self.url("inference")).multipart(form)
            },
        };
        let response = request
            .timeout(INFERENCE_TIMEOUT)
            .send()
            .await
            // The request URL carries the private route, so the transport error never reaches users.
            .map_err(|error| {
                if error.is_timeout() {
                    anyhow!("Transcription timed out. Try a smaller model or GPU acceleration.")
                } else {
                    anyhow!("Local transcription stopped. Check the model/backend and try again.")
                }
            })?;
        ensure!(
            response.status().is_success(),
            "Local speech returned HTTP {}",
            response.status().as_u16()
        );
        let body = read_bounded(response).await?;
        let reply: Transcription =
            serde_json::from_slice(&body).context("Local speech returned invalid JSON")?;
        reply.text.context("Local speech returned no transcript")
    }

    async fn probe_with_silence(&self) -> anyhow::Result<()> {
        let silence = audio::silent_wav(16_000, Duration::from_secs(1))?;
        self.transcribe(silence, "en").await?;
        Ok(())
    }

    async fn stop(&mut self) {
        self.group.terminate();
        kill_and_reap(&mut self.child).await;
    }
}

/// The part of either engine's JSON reply that Speakeasy reads.
#[derive(Deserialize)]
struct Transcription {
    text: Option<String>,
}

fn server_command(config: &Config, port: u16, secret: &str, served: &Path) -> Command {
    let port = port.to_string();
    let mut command = owned_command(&config.engine_executable);
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
    // Engines may print dictated text; diagnostics report only the exit status.
    command.stdout(Stdio::null());
    command.process_group(0);
    command
}

fn recording_form(wav: Vec<u8>) -> reqwest::Result<multipart::Form> {
    let file = multipart::Part::bytes(wav)
        .file_name("dictation.wav")
        .mime_str("audio/wav")?;
    Ok(multipart::Form::new()
        .part("file", file)
        .text("response_format", "json"))
}

async fn read_bounded(mut response: Response) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            chunk.len() <= MAX_TEXT_BYTES.saturating_sub(body.len()),
            "Local speech response exceeded the size limit"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn startup_exit(status: ExitStatus) -> String {
    format!(
        "Local speech exited before becoming ready ({status}). {}",
        exit_remedy(status.code(), status.signal())
    )
}

fn exit_remedy(code: Option<i32>, signal: Option<i32>) -> &'static str {
    match (code.map(i32::cast_unsigned), signal) {
        (_, Some(SIGILL)) => {
            "This engine uses CPU instructions unavailable on this machine. Choose a compatible engine executable in Settings."
        },
        (Some(USAGE_ERROR), _) => {
            "Check that the selected engine supports Speakeasy's server arguments. Run automatic setup to install the supported engine, or choose its executable in Settings."
        },
        _ => {
            "Check the selected model and engine dependencies. Run automatic setup to install the supported engine, or choose a compatible executable in Settings."
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_diagnostics_identify_actions_without_engine_output() {
        assert!(exit_remedy(Some(2), None).contains("server arguments"));
        assert!(exit_remedy(None, Some(SIGILL)).contains("CPU instructions"));
        assert!(exit_remedy(Some(1), None).contains("selected model"));
    }

    #[test]
    fn server_arguments_match_each_engine() {
        let arguments = |config: &Config| {
            server_command(config, 8080, "secret", Path::new("served"))
                .as_std()
                .get_args()
                .filter_map(|argument| argument.to_str().map(str::to_owned))
                .collect::<Vec<_>>()
        };
        let whisper = Config {
            model: "model.bin".into(),
            use_gpu: false,
            ..Config::default()
        };
        assert_eq!(
            arguments(&whisper),
            [
                "--model",
                "model.bin",
                "--host",
                "127.0.0.1",
                "--port",
                "8080",
                "--request-path",
                "/secret",
                "--public",
                "served",
                "--threads",
                "4",
                "--language",
                "en",
                "--no-gpu",
            ],
            "Whisper keeps default timestamp decoding and ends with its CPU switch"
        );
        let whisper_on_gpu = Config {
            use_gpu: true,
            ..whisper
        };
        assert_eq!(
            arguments(&whisper_on_gpu).last().map(String::as_str),
            Some("en")
        );
        let parakeet = Config {
            engine: Engine::Parakeet,
            model: "model.nemo".into(),
            ..Config::default()
        };
        assert_eq!(
            arguments(&parakeet),
            [
                "serve",
                "--asr-model",
                "model.nemo",
                "--device",
                "auto",
                "--host",
                "127.0.0.1",
                "--port",
                "8080",
                "--no-ui",
                "--threads",
                "1",
                "--asr.batching.enabled=false",
            ]
        );
    }

    fn fake_engine(script: &str) -> anyhow::Result<(tempfile::TempDir, Config)> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("worker");
        std::fs::write(&executable, script)?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        let config = Config {
            engine_executable: executable,
            use_gpu: false,
            ..Config::default()
        };
        Ok((directory, config))
    }

    #[tokio::test]
    async fn argument_failure_reports_exit_status_and_keeps_stderr_private() -> anyhow::Result<()> {
        let (_directory, config) =
            fake_engine("#!/bin/sh\nprintf 'private transcript or credential' >&2\nexit 2\n")?;
        let (cancel, cancelled) = watch::channel(false);
        let result = Server::start(config, cancelled).await;
        drop(cancel);
        let error = result
            .err()
            .context("Engine unexpectedly became ready")?
            .to_string();
        assert!(error.contains("exit status: 2"));
        assert!(error.contains("server arguments"));
        assert!(!error.contains("private transcript"));
        Ok(())
    }

    #[tokio::test]
    async fn startup_cancellation_reaps_the_owned_process() -> anyhow::Result<()> {
        let (directory, config) = fake_engine(
            "#!/bin/sh\nprintf '%s' \"$$\" > \"$(dirname \"$0\")/worker.pid\"\nexec sleep 60\n",
        )?;
        let (cancel, cancelled) = watch::channel(false);
        let task = tokio::spawn(Server::start(config, cancelled));
        let pid = timeout(Duration::from_secs(5), fixture_pid(directory.path())).await?;
        cancel.send_replace(true);
        assert!(
            timeout(Duration::from_secs(5), task)
                .await?
                .is_ok_and(|result| result.is_err())
        );
        // `kill -0` succeeds for a zombie too, so failure proves the worker was reaped.
        let probe = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()?;
        assert!(
            !probe.success(),
            "Cancelled startup left a live or unreaped worker"
        );
        Ok(())
    }

    async fn fixture_pid(directory: &Path) -> u32 {
        loop {
            if let Ok(pid) = tokio::fs::read_to_string(directory.join("worker.pid")).await
                && let Ok(pid) = pid.parse()
            {
                return pid;
            }
            sleep(Duration::from_millis(5)).await;
        }
    }
}
