use crate::config::{Config, Engine};
use anyhow::{Context, bail};
use reqwest::{Client, multipart};
use std::{net::TcpListener, process::Stdio, time::Duration};
use tokio::{
    process::{Child, Command},
    sync::watch,
    time::{sleep, timeout},
};

pub(crate) struct LocalSpeech {
    engine: Engine,
    group: speakeasy_platform::ProcessGroup,
    child: Child,
    endpoint: String,
    client: Client,
    // Whisper serves an empty directory. Its random basename also supplies the
    // private Whisper route or the loopback NeMo API key for this owned process.
    directory: tempfile::TempDir,
}

impl LocalSpeech {
    pub(crate) async fn start(
        config: Config,
        mut cancelled: watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!*cancelled.borrow(), "Local model startup cancelled");
        let engine = config.engine;
        let nemo = engine == Engine::Parakeet;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let directory = tempfile::Builder::new().prefix("speakeasy-").tempdir()?;
        let route = directory
            .path()
            .file_name()
            .context("Missing temporary directory name")?
            .to_string_lossy()
            .into_owned();
        let mut command = Command::new(&config.engine_executable);
        if nemo {
            command
                .args(["serve", "--asr-model"])
                .arg(&config.model)
                .args([
                    "--device",
                    if config.use_gpu { "auto" } else { "cpu" },
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--no-ui",
                    "--threads",
                    "1",
                    "--asr.batching.enabled=false",
                ])
                .env("NEMO_SPEECH_HTTP_API_KEY", &route);
        } else {
            // Keep default timestamp decoding: disabling it can skip speech
            // when a window ends early. The JSON response contains plain text.
            command
                .args(["--model"])
                .arg(&config.model)
                .args([
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--request-path",
                    &format!("/{route}"),
                    "--public",
                ])
                .arg(directory.path())
                .args([
                    "--threads",
                    &config.threads.to_string(),
                    "--language",
                    &config.language,
                ]);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if !config.use_gpu && !nemo {
            command.arg("--no-gpu");
        }
        #[cfg(target_os = "windows")]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        drop(listener);
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .build()?;
        let mut child = command.spawn().context(
            "Cannot start local speech. Check the selected engine executable and its native dependencies.",
        )?;
        let group = match speakeasy_platform::ProcessGroup::attach(
            child.id().context("Worker exited before containment")?,
        ) {
            Ok(group) => group,
            Err(error) => {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Containment failure remains the actionable error; kill-on-drop still owns a child that exits during cleanup"
                )]
                let _ = child.start_kill();
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Reap the failed child before returning its containment error, including an already-reaped child"
                )]
                let _ = child.wait().await;
                return Err(error);
            },
        };
        let mut server = Self {
            engine,
            group,
            child,
            endpoint: if nemo {
                format!("http://127.0.0.1:{port}")
            } else {
                format!("http://127.0.0.1:{port}/{route}")
            },
            client,
            directory,
        };
        let readiness = tokio::select! {
            biased;
            _ = cancelled.changed() => Err(anyhow::anyhow!("Local model startup cancelled")),
            result = timeout(Duration::from_secs(120), async {
            loop {
                if let Some(status) = server.child.try_wait()? {
                    #[cfg(unix)]
                    server.group.disarm();
                    bail!("{}", startup_exit(status));
                }
                if let Ok(response) = server
                    .client
                    .get(format!("{}/{}", server.endpoint, if nemo { "ready" } else { "health" }))
                    .timeout(Duration::from_millis(200))
                    .send()
                    .await
                    && response.status().is_success()
                {
                    return Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        => result.context("Local model startup timed out")
        .and_then(|result| result),
        };
        if let Err(error) = readiness {
            server.stop().await;
            return Err(error);
        }
        if config.use_gpu || cfg!(target_os = "linux") {
            // Loading weights does not initialize every GPU kernel or prove a
            // Linux binary can execute its CPU kernels. Exercise inference with
            // one second of synthetic silence before publishing readiness.
            let (timed_out, failed) = if config.use_gpu {
                (
                    "Local GPU warmup timed out. Check the selected engine and GPU dependencies.",
                    "Local GPU warmup failed. Check the selected engine and GPU dependencies.",
                )
            } else {
                (
                    "Local CPU inference timed out. Choose a smaller model or a compatible engine executable in Settings.",
                    "Local CPU inference failed. Check the model and CPU compatibility; choose a compatible engine executable in Settings.",
                )
            };
            let warmup = tokio::select! {
                biased;
                _ = cancelled.changed() => Err(anyhow::anyhow!("Local model startup cancelled")),
                result = timeout(Duration::from_secs(120), server.idle()) =>
                    result.context(timed_out).and_then(|result| result.context(failed)),
            };
            if let Err(error) = warmup {
                server.stop().await;
                return Err(error);
            }
        }
        if *cancelled.borrow() {
            server.stop().await;
            bail!("Local model startup cancelled");
        }
        Ok(server)
    }

    pub(crate) async fn idle(&self) -> anyhow::Result<()> {
        // Whisper serializes inference with its model lock; NeMo uses one HTTP
        // worker with batching disabled. A completed silent request waits behind
        // disconnected inference and confirms the model remains usable. Health
        // endpoints cannot provide that barrier. Discard the response.
        let silence = crate::audio::wave(vec![0; 44 + 16_000 * 2], 16_000)?;
        self.transcribe(silence, "en").await?;
        Ok(())
    }

    pub(crate) async fn transcribe(&self, wav: Vec<u8>, language: &str) -> anyhow::Result<String> {
        if self.engine == Engine::Parakeet {
            // Capture writes the fixed PCM header in audio::wave; no external
            // container parsing or resampling is needed at this boundary.
            let rate = wav
                .get(24..28)
                .and_then(|bytes| bytes.try_into().ok())
                .map(u32::from_le_bytes)
                .context("Missing recording sample rate")?;
            if !(8_000..=96_000).contains(&rate) {
                bail!(
                    "Parakeet needs 8–96 kHz audio. Set your microphone to 48 kHz in OS settings or choose Whisper."
                );
            }
        }
        let mut form = multipart::Form::new()
            .part(
                "file",
                multipart::Part::bytes(wav)
                    .file_name("dictation.wav")
                    .mime_str("audio/wav")?,
            )
            .text("response_format", "json");
        if self.engine == Engine::Whisper {
            form = form
                .text("language", language.to_owned())
                .text("temperature", "0.0");
        }
        let path = if self.engine == Engine::Parakeet {
            "v1/audio/transcriptions"
        } else {
            "inference"
        };
        let mut request = self.client.post(format!("{}/{path}", self.endpoint));
        if self.engine == Engine::Parakeet {
            request = request.bearer_auth(
                self.directory
                    .path()
                    .file_name()
                    .context("Missing key")?
                    .to_string_lossy(),
            );
        }
        let mut response = request
            .multipart(form)
            // A five-minute recording on a CPU can exceed two minutes of work.
            // Escape drops the request; bounded recovery kills an unresponsive worker.
            .timeout(Duration::from_mins(30))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    anyhow::anyhow!(
                        "Transcription timed out. Try a smaller model or GPU acceleration."
                    )
                } else {
                    anyhow::anyhow!(
                        "Local transcription stopped. Check the model/backend and try again."
                    )
                }
            })?;
        if !response.status().is_success() {
            bail!("Local speech returned HTTP {}", response.status().as_u16());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > (1024 * 1024_usize).saturating_sub(bytes.len()) {
                bail!("Local speech response exceeded the size limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        let result: serde_json::Value =
            serde_json::from_slice(&bytes).context("Local speech returned invalid JSON")?;
        result
            .get("text")
            .and_then(|text| text.as_str())
            // Server segments contain layout newlines, not dictated Enter keys.
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            .context("Local speech returned no transcript")
    }

    pub(crate) async fn stop(&mut self) {
        self.group.terminate();
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Termination is idempotent and the process group already owns remaining children; still reap an exited leader"
        )]
        let _ = self.child.start_kill();
        // Replacement and Pause wait for actual process exit. A timeout here
        // would hand the OS an unreaped child while a new model starts.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Waiting releases the child even when it was already reaped; worker retirement cannot update an abandoned session"
        )]
        let _ = self.child.wait().await;
    }
}

// Exit status carries no engine output, credentials, audio, or transcript. Keep
// stderr discarded: a private engine may print dictated text there after ready.
fn startup_exit(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    let illegal_instruction = {
        use std::os::unix::process::ExitStatusExt;
        status.signal() == Some(4)
    }; // SIGILL on supported Unix platforms
    #[cfg(not(unix))]
    let illegal_instruction = false;
    format!(
        "Local speech exited before becoming ready ({status}). {}",
        exit_remedy(status.code(), illegal_instruction)
    )
}
fn exit_remedy(code: Option<i32>, illegal_instruction: bool) -> &'static str {
    match code.map(i32::cast_unsigned) {
        code if illegal_instruction || code == Some(0xc000_001d) => {
            "This engine uses CPU instructions unavailable on this machine. Choose a compatible engine executable in Settings."
        },
        Some(0xc000_0135 | 0xc000_0139 | 0xc000_007b) => {
            "This engine needs missing or incompatible native libraries. Run automatic setup or install the matching engine dependencies."
        },
        Some(2) => {
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
        assert!(exit_remedy(Some(2), false).contains("server arguments"));
        assert!(exit_remedy(None, true).contains("CPU instructions"));
        assert!(
            exit_remedy(Some(0xc000_001d_u32.cast_signed()), false).contains("CPU instructions")
        );
        for code in [0xc000_0135_u32, 0xc000_0139, 0xc000_007b] {
            assert!(exit_remedy(Some(code.cast_signed()), false).contains("native libraries"));
        }
        assert!(exit_remedy(Some(1), false).contains("selected model"));
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn argument_failure_reports_exit_status_and_keeps_stderr_private() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("worker");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf 'private transcript or credential' >&2\nexit 2\n",
        )?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        let (cancel, cancelled) = watch::channel(false);
        let result = LocalSpeech::start(
            Config {
                engine_executable: executable,
                use_gpu: false,
                ..Config::default()
            },
            cancelled,
        )
        .await;
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn startup_cancellation_reaps_the_owned_process() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join("worker");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s' \"$$\" > \"$(dirname \"$0\")/worker.pid\"\nexec sleep 60\n",
        )?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        let config = Config {
            engine_executable: executable,
            use_gpu: false,
            ..Config::default()
        };
        let (cancel, cancelled) = watch::channel(false);
        let task = tokio::spawn(LocalSpeech::start(config, cancelled));
        let pid = timeout(Duration::from_secs(5), fixture_pid(directory.path())).await?;
        cancel.send_replace(true);
        assert!(
            timeout(Duration::from_secs(5), task)
                .await?
                .is_ok_and(|result| result.is_err())
        );
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "Cancelled startup left a live or unreaped worker"
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn fixture_pid(directory: &std::path::Path) -> u32 {
        loop {
            if let Ok(pid) = tokio::fs::read_to_string(directory.join("worker.pid")).await
                && let Ok(pid) = pid.parse()
            {
                return pid;
            }
            sleep(Duration::from_millis(5)).await;
        }
    }

    // A real provider check, separate from the fast portable tests. The caller
    // supplies public fixture audio; this never opens a microphone or clipboard.
    #[tokio::test]
    #[ignore = "Requires SPEAKEASY_FIXTURE_CONFIG and SPEAKEASY_FIXTURE_WAV (whisper.cpp samples/jfk.wav)"]
    async fn local_worker_recognizes_fixture_and_stops() -> anyhow::Result<()> {
        let config = Config::load(std::path::Path::new(&std::env::var(
            "SPEAKEASY_FIXTURE_CONFIG",
        )?))?;
        let wav = std::fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
        let language = config.language.clone();
        let started = std::time::Instant::now();
        let (_cancel, cancelled) = watch::channel(false);
        let mut worker = LocalSpeech::start(config, cancelled).await?;
        let startup = started.elapsed();
        let started = std::time::Instant::now();
        let result = worker.transcribe(wav, &language).await?;
        eprintln!(
            "Provider startup: {startup:?}; inference: {:?}",
            started.elapsed()
        );
        // Never include the recognized text in failure output.
        assert!(
            result.to_lowercase().contains("ask not what your country"),
            "Fixture recognition did not contain the expected phrase"
        );
        assert!(
            !result.contains(['\r', '\n']),
            "Segment boundaries became editor input"
        );
        let stopped = std::time::Instant::now();
        worker.stop().await;
        eprintln!("Provider shutdown: {:?}", stopped.elapsed());
        assert!(
            worker.child.try_wait()?.is_some(),
            "Owned worker did not terminate"
        );
        Ok(())
    }
}
