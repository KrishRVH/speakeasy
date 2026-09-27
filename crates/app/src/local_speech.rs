use crate::config::{Config, Engine};
use anyhow::{Context, bail};
use reqwest::{Client, multipart};
use std::{net::TcpListener, process::Stdio, time::Duration};
use tokio::{
    process::{Child, Command},
    time::{sleep, timeout},
};

pub struct LocalSpeech {
    engine: Engine,
    group: speakeasy_platform::ProcessGroup,
    child: Child,
    endpoint: String,
    client: Client,
    // Whisper serves an empty directory. Its random basename also supplies the
    // private Whisper route or the loopback NeMo API key for this owned process.
    _directory: tempfile::TempDir,
}

impl LocalSpeech {
    pub async fn start(config: Config) -> anyhow::Result<Self> {
        let engine = config.engine;
        let nemo = engine == Engine::Parakeet;
        if nemo && !config.use_gpu {
            bail!("Parakeet requires GPU acceleration. Enable Prefer GPU or select Whisper.");
        }
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
                    "--gpu",
                    "0",
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
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        drop(listener);
        let child = command.spawn().context(
            "Cannot start local speech. Check the selected engine executable and its native dependencies.",
        )?;
        let group = speakeasy_platform::ProcessGroup::attach(
            child.id().context("Worker exited before containment")?,
        )?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .build()?;
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
            _directory: directory,
        };
        timeout(Duration::from_secs(120), async {
            loop {
                if server.child.try_wait()?.is_some() {
                    #[cfg(unix)]
                    server.group.disarm();
                    bail!("Local speech stopped. Check the model, CPU compatibility, and GPU dependencies.");
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
        .await
        .context("Local model startup timed out")??;
        if config.use_gpu {
            // Loading weights does not initialize every GPU kernel. Exercise the
            // normal request path while loading, using one second of synthetic
            // silence. The response is discarded and never reaches insertion.
            timeout(Duration::from_secs(120), server.idle())
                .await
                .context(
                    "Local GPU warmup timed out. Check the selected engine and GPU dependencies.",
                )?
                .context(
                    "Local GPU warmup failed. Check the selected engine and GPU dependencies.",
                )?;
        }
        Ok(server)
    }

    pub async fn idle(&self) -> anyhow::Result<()> {
        // Whisper serializes inference with its model lock; NeMo uses one HTTP
        // worker with batching disabled. A completed silent request waits behind
        // disconnected inference and confirms the model remains usable. Health
        // endpoints cannot provide that barrier. Discard the response.
        let silence = crate::audio::wave(vec![0; 44 + 16_000 * 2], 16_000);
        self.transcribe(silence, "en").await?;
        Ok(())
    }

    pub async fn transcribe(&self, wav: Vec<u8>, language: &str) -> anyhow::Result<String> {
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
                self._directory
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
            .timeout(Duration::from_secs(30 * 60))
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
            if bytes.len() + chunk.len() > 1024 * 1024 {
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

    pub async fn stop(&mut self) {
        self.group.terminate();
        let _ = self.child.start_kill();
        let _ = timeout(Duration::from_secs(3), self.child.wait()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut worker = LocalSpeech::start(config).await?;
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
