//! Parakeet in this executable's helper mode: the configured installation's C library holds the
//! model in a child process that exchanges PCM16 and text with this one over its pipes.
//!
//! One task owns the pipes and answers requests in order, so the reply to an abandoned request is
//! read and discarded rather than mistaken for the next request's.

mod child;
mod frame;

use std::{io, path::PathBuf, process::Stdio, time::Duration};

use anyhow::{Context, anyhow, bail, ensure};
use speakeasy_platform::{ProcessGroup, speech::Failure};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::Child,
    sync::{oneshot, watch},
    task::JoinHandle,
    time::timeout,
};

pub use self::child::{HELPER_FLAG, run_helper};
use self::{
    child::{PROTOCOL, PROTOCOL_MISMATCH},
    frame::Request,
};
use super::{
    INFERENCE_TIMEOUT, MAX_TEXT_BYTES, STARTUP_TIMEOUT, Warmup, contain, unless_cancelled,
};
use crate::{
    audio,
    child::{kill_and_reap, owned_command},
    config::Config,
    ports::{STARTUP_CANCELLED, Speech},
};

/// The engine's C library, relative to its installation root, the parent of `bin/`.
const LIBRARY: &str = "lib/libnemo_speech_asr_c.1.dylib";
const STOPPED: &str = "Local transcription stopped. Check the model and engine, then try again.";

/// The configured installation's C library, if it ships one.
pub(super) fn library(config: &Config) -> Option<PathBuf> {
    let library = config.engine_executable.parent()?.parent()?.join(LIBRARY);
    library.is_file().then_some(library)
}

pub(crate) struct Helper {
    exchanges: async_channel::Sender<Exchange>,
    pipes: JoinHandle<()>,
    /// Declared before `child`, so dropping terminates the group before its leader can be reaped.
    group: ProcessGroup,
    child: Child,
}

/// One request and where its outcome goes; a dropped receiver discards the reply.
struct Exchange {
    rate: u32,
    wav: Erased,
    reply: oneshot::Sender<anyhow::Result<String>>,
}

/// A recording erased, best effort, however its request ends: answered, queued when the helper
/// stops, or in flight when the pipe task is aborted.
struct Erased(Vec<u8>);

impl Drop for Erased {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl Helper {
    pub(super) async fn start(
        config: Config,
        library: PathBuf,
        mut cancelled: watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        ensure!(!*cancelled.borrow(), STARTUP_CANCELLED);
        let mut command = owned_command(std::env::current_exe()?);
        command
            .arg(HELPER_FLAG)
            .arg(PROTOCOL)
            .arg(library)
            .arg(&config.model)
            .arg(if config.use_gpu { "gpu" } else { "cpu" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .process_group(0);
        let mut child = command
            .spawn()
            .context("Cannot start local speech. Check the selected engine and model.")?;
        let mut group = contain(&mut child).await?;
        let pipes = child.stdin.take().zip(child.stdout.take());
        let ready = match pipes {
            Some((input, output)) => {
                let mut output = BufReader::new(output);
                unless_cancelled(&mut cancelled, async {
                    timeout(STARTUP_TIMEOUT, loaded(&mut output))
                        .await
                        .context("Local model startup timed out")?
                })
                .await
                .map(|()| (input, output))
            },
            None => Err(anyhow!(STOPPED)),
        };
        let (input, output) = match ready {
            Ok(pipes) => pipes,
            Err(error) => {
                group.terminate();
                kill_and_reap(&mut child).await;
                let updated = matches!(
                    child.try_wait(),
                    Ok(Some(status)) if status.code() == Some(PROTOCOL_MISMATCH)
                );
                if updated {
                    bail!(
                        "Speakeasy was updated while running. Quit and reopen it to load the model."
                    );
                }
                return Err(error);
            },
        };
        let (exchanges, received) = async_channel::unbounded();
        let mut helper = Self {
            exchanges,
            pipes: tokio::spawn(exchange(input, output, received)),
            group,
            child,
        };
        if let Some(warmup) = Warmup::required(&config) {
            let warmed = unless_cancelled(&mut cancelled, async {
                timeout(STARTUP_TIMEOUT, helper.probe_with_silence())
                    .await
                    .context(warmup.timed_out)?
                    .context(warmup.failed)
            })
            .await;
            if let Err(error) = warmed {
                helper.stop().await;
                return Err(error);
            }
        }
        Ok(helper)
    }
}

impl Speech for Helper {
    async fn transcribe(&self, wav: Vec<u8>, _: &str) -> anyhow::Result<String> {
        let wav = Erased(wav);
        let rate = audio::wav_sample_rate(&wav.0).context("Missing recording sample rate")?;
        let (reply, outcome) = oneshot::channel();
        let exchange = Exchange { rate, wav, reply };
        if self.exchanges.send(exchange).await.is_err() {
            bail!(STOPPED);
        }
        match timeout(INFERENCE_TIMEOUT, outcome).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => bail!(STOPPED),
            Err(_) => {
                bail!("Transcription timed out. Try GPU acceleration or a shorter recording.")
            },
        }
    }

    async fn probe_with_silence(&self) -> anyhow::Result<()> {
        let silence = audio::silent_wav(16_000, Duration::from_secs(1))?;
        self.transcribe(silence, "en").await?;
        Ok(())
    }

    async fn stop(&mut self) {
        self.exchanges.close();
        self.pipes.abort();
        self.group.terminate();
        kill_and_reap(&mut self.child).await;
    }
}

/// Waits for the status byte the helper writes once its model has loaded.
async fn loaded(output: &mut (impl AsyncRead + Unpin)) -> anyhow::Result<()> {
    let Ok(status) = output.read_u8().await else {
        bail!(
            "Local speech stopped while loading the model. Check the model in Settings or run automatic setup again."
        );
    };
    Ok(frame::outcome(status)??)
}

/// Owns both pipes: writes each request, reads its reply, and hands the reply to whoever still
/// waits. A broken stream ends the exchange, failing every later request.
async fn exchange(
    mut input: impl AsyncWrite + Unpin,
    mut output: impl AsyncRead + Unpin,
    exchanges: async_channel::Receiver<Exchange>,
) {
    while let Ok(Exchange { rate, wav, reply }) = exchanges.recv().await {
        let request =
            audio::wav_pcm(&wav.0).and_then(|pcm| Some((Request::for_pcm(rate, pcm)?, pcm)));
        let outcome = match request {
            None => Ok(Err(Failure::Audio)),
            Some((request, pcm)) => round_trip(&mut input, &mut output, request, pcm).await,
        };
        drop(wav);
        let broken = outcome.is_err();
        drop(reply.send(match outcome {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(failure)) => Err(failure.into()),
            Err(_) => Err(anyhow!(STOPPED)),
        }));
        if broken {
            return;
        }
    }
}

async fn round_trip(
    input: &mut (impl AsyncWrite + Unpin),
    output: &mut (impl AsyncRead + Unpin),
    request: Request,
    pcm: &[u8],
) -> io::Result<Result<String, Failure>> {
    input.write_all(&request.encode()).await?;
    input.write_all(pcm).await?;
    input.flush().await?;
    if let Err(failure) = frame::outcome(output.read_u8().await?)? {
        return Ok(Err(failure));
    }
    let length = usize::try_from(output.read_u32_le().await?).unwrap_or(usize::MAX);
    if length > MAX_TEXT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Transcript exceeds the size limit",
        ));
    }
    let mut text = vec![0; length];
    output.read_exact(&mut text).await?;
    match String::from_utf8(text) {
        Ok(text) => Ok(Ok(text)),
        Err(_) => Err(io::ErrorKind::InvalidData.into()),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{DuplexStream, duplex};

    use super::*;

    /// The helper's end of both pipes.
    struct FakeHelper {
        requests: DuplexStream,
        replies: DuplexStream,
    }

    impl FakeHelper {
        /// Reads one request, returning its header and samples.
        async fn request(&mut self) -> anyhow::Result<(Request, Vec<u8>)> {
            let mut header = [0; Request::BYTES];
            self.requests.read_exact(&mut header).await?;
            let request = Request::decode(header)?;
            let mut pcm = vec![0; request.pcm_bytes()];
            self.requests.read_exact(&mut pcm).await?;
            Ok((request, pcm))
        }

        async fn reply(&mut self, text: &str) -> anyhow::Result<()> {
            self.replies.write_u8(frame::READY).await?;
            self.replies
                .write_u32_le(u32::try_from(text.len())?)
                .await?;
            self.replies.write_all(text.as_bytes()).await?;
            Ok(())
        }
    }

    fn start() -> (async_channel::Sender<Exchange>, FakeHelper) {
        let (input, requests) = duplex(1 << 16);
        let (replies, output) = duplex(1 << 16);
        let (exchanges, received) = async_channel::unbounded();
        tokio::spawn(exchange(input, output, received));
        (exchanges, FakeHelper { requests, replies })
    }

    async fn send(
        exchanges: &async_channel::Sender<Exchange>,
        samples: &[i16],
    ) -> anyhow::Result<oneshot::Receiver<anyhow::Result<String>>> {
        let mut wav = audio::silent_wav(48_000, Duration::ZERO)?;
        wav.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
        let (reply, outcome) = oneshot::channel();
        exchanges
            .send(Exchange {
                rate: 48_000,
                wav: Erased(wav),
                reply,
            })
            .await?;
        Ok(outcome)
    }

    #[tokio::test]
    async fn requests_carry_raw_pcm_and_replies_reach_their_own_caller() -> anyhow::Result<()> {
        let (exchanges, mut helper) = start();
        let abandoned = send(&exchanges, &[1, -2]).await?;
        let (request, pcm) = helper.request().await?;
        assert_eq!(
            request,
            Request {
                rate: 48_000,
                samples: 2
            }
        );
        assert_eq!(pcm, [1, 0, 0xFE, 0xFF]);
        drop(abandoned);
        let current = send(&exchanges, &[3]).await?;
        helper.reply("abandoned").await?;
        helper.request().await?;
        helper.reply("current").await?;
        assert_eq!(current.await??, "current");
        Ok(())
    }

    #[tokio::test]
    async fn engine_failures_keep_the_stream_and_broken_replies_end_it() -> anyhow::Result<()> {
        let (exchanges, mut helper) = start();
        let failed = send(&exchanges, &[1]).await?;
        helper.request().await?;
        helper.replies.write_u8(Failure::OutOfMemory as u8).await?;
        let message = failed.await?.unwrap_err().to_string();
        assert_eq!(message, Failure::OutOfMemory.to_string());
        let corrupt = send(&exchanges, &[1]).await?;
        helper.request().await?;
        helper.replies.write_u8(0xFF).await?;
        assert_eq!(corrupt.await?.unwrap_err().to_string(), STOPPED);
        let after = send(&exchanges, &[1]).await;
        assert!(
            after.is_err() || after?.await.is_err(),
            "A broken stream kept accepting requests"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_unloadable_library_stays_identifiable_for_the_server_fallback() {
        let mut output: &[u8] = &[Failure::Library as u8];
        let error = loaded(&mut output).await.unwrap_err();
        assert_eq!(error.downcast_ref::<Failure>(), Some(&Failure::Library));
    }

    #[test]
    fn only_an_installation_with_the_c_library_runs_in_the_helper() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let executable = root.path().join("nemo-speech/bin/nemo-speech");
        std::fs::create_dir_all(root.path().join("nemo-speech/lib"))?;
        let config = Config {
            engine_executable: executable,
            ..Config::default()
        };
        assert_eq!(library(&config), None);
        let path = root
            .path()
            .join("nemo-speech/lib/libnemo_speech_asr_c.1.dylib");
        std::fs::write(&path, b"fixture")?;
        assert_eq!(library(&config), Some(path));
        Ok(())
    }
}
