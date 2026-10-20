//! Pinned downloads: each file is fetched by URL, checked against its published size and SHA-256,
//! resumed from its partial file after an interruption, and renamed into place only once verified.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, bail};
use reqwest::{Client, StatusCode, header::RANGE};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncReadExt, sync::watch};

use super::Progress;

pub(super) struct Download<'a> {
    pub(super) url: &'a str,
    pub(super) size: u64,
    pub(super) sha256: &'a str,
}

/// Downloads into `directory`, resuming a partial file when the server honors the range. Returns
/// the verified file.
pub(super) async fn fetch(
    client: &Client,
    download: &Download<'_>,
    directory: &Path,
    step: &'static str,
    progress: &watch::Sender<Progress>,
) -> anyhow::Result<PathBuf> {
    let name = file_name(download.url);
    let path = directory.join(name);
    if path.is_file() {
        return Ok(path);
    }
    fs::create_dir_all(directory)
        .with_context(|| format!("Cannot write to {}", directory.display()))?;
    let partial = directory.join(format!("{name}.part"));
    let mut done = fs::metadata(&partial).map_or(0, |metadata| metadata.len());
    if done == download.size {
        let hasher = hash_partial(&partial, done, progress).await?;
        if hex_digest(&hasher.finalize()) == download.sha256 {
            fs::rename(&partial, &path)?;
            return Ok(path);
        }
        fs::remove_file(&partial)?;
        done = 0;
    } else if done > download.size {
        done = 0;
    }
    let mut request = client.get(download.url);
    if done > 0 {
        request = request.header(RANGE, format!("bytes={done}-"));
    }
    let mut response = request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .context("Cannot download. Check your internet connection, then try again.")?;
    let (mut hasher, mut file) = if done > 0 && response.status() == StatusCode::PARTIAL_CONTENT {
        (
            hash_partial(&partial, done, progress).await?,
            OpenOptions::new().append(true).open(&partial)?,
        )
    } else {
        done = 0;
        (Sha256::new(), File::create(&partial)?)
    };
    progress.send_replace(Progress {
        step: Some(step),
        done,
        total: download.size,
    });
    while let Some(chunk) = response
        .chunk()
        .await
        .context("The download stopped. Check your internet connection, then try again.")?
    {
        done = done
            .checked_add(chunk.len() as u64)
            .context("The download exceeded its published size. Try again.")?;
        if done > download.size {
            break;
        }
        hasher.update(&chunk);
        file.write_all(&chunk)?;
        progress.send_modify(|progress| progress.done = done);
    }
    file.sync_all()?;
    drop(file);
    if done != download.size || hex_digest(&hasher.finalize()) != download.sha256 {
        fs::remove_file(&partial).context(
            "Cannot remove an invalid download. Check directory permissions, then try again.",
        )?;
        bail!("A download did not match its published checksum. Try again.");
    }
    fs::rename(&partial, &path)?;
    Ok(path)
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0xF])
        .filter_map(|digit| char::from_digit(digit.into(), 16))
        .collect()
}

async fn hash_partial(
    partial: &Path,
    size: u64,
    progress: &watch::Sender<Progress>,
) -> anyhow::Result<Sha256> {
    progress.send_replace(Progress {
        step: Some("Verifying downloaded data"),
        done: 0,
        total: size,
    });
    let mut file = tokio::fs::File::open(partial).await?;
    // Every asynchronous read is a blocking-pool round trip, so read a MiB at a time.
    let mut buffer = vec![0; 1024 * 1024];
    let mut hasher = Sha256::new();
    loop {
        let count = file.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let chunk = buffer
            .get(..count)
            .context("Cannot verify the downloaded file: the read size is invalid")?;
        hasher.update(chunk);
        progress.send_modify(|progress| {
            progress.done = progress.done.saturating_add(count as u64);
        });
        // Cached reads can complete without yielding; let cancellation run between chunks.
        tokio::task::yield_now().await;
    }
    Ok(hasher)
}

pub(super) fn file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, time::Duration};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        time::timeout,
    };

    use super::*;
    use crate::{http::client_builder, setup::Installation};

    const BODY: &[u8] = b"Say the word. The door opens, and what is said stays inside.";

    /// Serves `BODY` on loopback, honoring `Range: bytes=N-`. The receiver yields the range start
    /// each request asked for.
    async fn serve_body() -> anyhow::Result<(String, async_channel::Receiver<Option<u64>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let (requests, starts) = async_channel::unbounded();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if respond(stream, &requests).await.is_err() {
                    break;
                }
            }
        });
        Ok((url, starts))
    }

    async fn respond(
        mut stream: TcpStream,
        requests: &async_channel::Sender<Option<u64>>,
    ) -> anyhow::Result<()> {
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.ends_with(b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await?;
            anyhow::ensure!(read > 0, "request ended early");
            request.extend_from_slice(&buffer[..read]);
        }
        let start = String::from_utf8(request)?
            .to_lowercase()
            .lines()
            .find_map(|line| line.strip_prefix("range: bytes="))
            .and_then(|range| range.trim_end_matches('-').parse::<usize>().ok());
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The fixture still serves its response when a test deliberately discards the optional request observer"
        )]
        let _ = requests.send(start.map(|start| start as u64)).await;
        let head = match start {
            Some(start) => format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{}\r\n",
                BODY.len().saturating_sub(1),
                BODY.len()
            ),
            None => "HTTP/1.1 200 OK\r\n".into(),
        };
        let body = &BODY[start.unwrap_or(0)..];
        let head = format!(
            "{head}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(body).await?;
        Ok(())
    }

    fn sha256(bytes: &[u8]) -> String {
        hex_digest(&Sha256::digest(bytes))
    }

    #[tokio::test]
    async fn fetch_promotes_a_verified_complete_partial_without_a_request() -> anyhow::Result<()> {
        let (base, requests) = serve_body().await?;
        let url = format!("{base}/model.gguf");
        let sha = sha256(BODY);
        let download = Download {
            url: &url,
            size: BODY.len() as u64,
            sha256: &sha,
        };
        let directory = tempfile::tempdir()?;
        let partial = directory.path().join("model.gguf.part");
        fs::write(&partial, BODY)?;
        let (progress, _) = watch::channel(Progress::default());
        let path = fetch(
            &client_builder()?.build()?,
            &download,
            directory.path(),
            "fixture",
            &progress,
        )
        .await?;
        assert_eq!(fs::read(path)?, BODY);
        assert!(!partial.exists());
        assert!(
            requests.try_recv().is_err(),
            "Complete verified audio model was downloaded again"
        );
        Ok(())
    }

    #[tokio::test]
    async fn fetch_replaces_a_corrupt_complete_partial() -> anyhow::Result<()> {
        let (base, requests) = serve_body().await?;
        let url = format!("{base}/model.gguf");
        let sha = sha256(BODY);
        let download = Download {
            url: &url,
            size: BODY.len() as u64,
            sha256: &sha,
        };
        let directory = tempfile::tempdir()?;
        fs::write(
            directory.path().join("model.gguf.part"),
            vec![0; BODY.len()],
        )?;
        let (progress, _) = watch::channel(Progress::default());
        let path = fetch(
            &client_builder()?.build()?,
            &download,
            directory.path(),
            "fixture",
            &progress,
        )
        .await?;
        assert_eq!(fs::read(path)?, BODY);
        assert_eq!(requests.recv().await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_during_verification_keeps_the_partial_for_retry() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let partial = directory.path().join("model.gguf.part");
        let bytes = vec![0; 4 * 1024 * 1024];
        fs::write(&partial, &bytes)?;
        let sha = sha256(&bytes);
        let download = Download {
            url: "http://127.0.0.1:1/model.gguf",
            size: bytes.len() as u64,
            sha256: &sha,
        };
        let (progress, mut updates) = watch::channel(Progress::default());
        let (cancel, cancelled) = async_channel::bounded::<Infallible>(1);
        let cancel_when_verifying = async {
            updates
                .wait_for(|progress| {
                    progress.step == Some("Verifying downloaded data")
                        && progress.done > 0
                        && progress.done < progress.total
                })
                .await?;
            drop(cancel);
            Ok::<_, anyhow::Error>(())
        };
        let work = Installation::run(directory.path(), &progress, &cancelled, async |_| {
            fetch(
                &client_builder()?.build()?,
                &download,
                directory.path(),
                "fixture",
                &progress,
            )
            .await
        });
        let (result, cancellation) = timeout(Duration::from_secs(5), async {
            tokio::join!(work, cancel_when_verifying)
        })
        .await?;
        cancellation?;
        assert!(result.is_none());
        assert_eq!(fs::read(partial)?, bytes);
        assert!(!directory.path().join("model.gguf").exists());
        Ok(())
    }

    #[tokio::test]
    async fn fetch_resumes_a_partial_download_and_verifies_the_whole_file() -> anyhow::Result<()> {
        let (base, requests) = serve_body().await?;
        let url = format!("{base}/model.gguf");
        let sha = sha256(BODY);
        let download = Download {
            url: &url,
            size: BODY.len() as u64,
            sha256: &sha,
        };
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("model.gguf.part"), &BODY[..20])?;
        let (progress, _) = watch::channel(Progress::default());
        let path = fetch(
            &client_builder()?.build()?,
            &download,
            directory.path(),
            "fixture",
            &progress,
        )
        .await?;
        assert_eq!(requests.recv().await?, Some(20));
        assert_eq!(fs::read(&path)?, BODY);
        assert!(!directory.path().join("model.gguf.part").exists());
        assert_eq!(progress.borrow().done, BODY.len() as u64);
        Ok(())
    }

    #[tokio::test]
    async fn fetch_discards_a_download_that_fails_its_checksum() -> anyhow::Result<()> {
        let (base, _) = serve_body().await?;
        let url = format!("{base}/model.gguf");
        let sha = sha256(b"a different model");
        let download = Download {
            url: &url,
            size: BODY.len() as u64,
            sha256: &sha,
        };
        let directory = tempfile::tempdir()?;
        let (progress, _) = watch::channel(Progress::default());
        let result = fetch(
            &client_builder()?.build()?,
            &download,
            directory.path(),
            "fixture",
            &progress,
        )
        .await;
        let error = result.err().map(|error| format!("{error:#}"));
        assert!(
            error
                .as_deref()
                .is_some_and(|error| error.contains("checksum")),
            "{error:?}"
        );
        assert!(!directory.path().join("model.gguf").exists());
        assert!(!directory.path().join("model.gguf.part").exists());
        Ok(())
    }
}
