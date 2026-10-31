//! Pinned cache integrity and resumable downloads over a synthetic loopback server.

use std::{convert::Infallible, time::Duration};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

use super::*;
use crate::{
    http::{Trust, client_builder},
    setup::Installation,
};

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
        anyhow::ensure!(read > 0, "Request ended early");
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

async fn fetch_into(
    download: &Download<'_>,
    directory: &Path,
    progress: &watch::Sender<Progress>,
) -> anyhow::Result<PathBuf> {
    fetch(
        &client_builder(Trust::Nobody)?.build()?,
        download,
        directory,
        "fixture",
        progress,
    )
    .await
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
    let path = fetch_into(&download, directory.path(), &progress).await?;
    assert_eq!(fs::read(path)?, BODY);
    assert!(!partial.exists());
    assert!(
        requests.is_empty(),
        "A verified complete partial was downloaded again"
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
    let path = fetch_into(&download, directory.path(), &progress).await?;
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
                progress.step == "Verifying downloaded data"
                    && progress.done > 0
                    && progress.done < progress.total
            })
            .await?;
        drop(cancel);
        Ok::<_, anyhow::Error>(())
    };
    let work = Installation::run(directory.path(), &progress, &cancelled, async |_| {
        fetch_into(&download, directory.path(), &progress).await
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
    let path = fetch_into(&download, directory.path(), &progress).await?;
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
    let error = fetch_into(&download, directory.path(), &progress)
        .await
        .err()
        .context("A download that failed its checksum was kept")?;
    assert!(format!("{error:#}").contains("checksum"), "{error:#}");
    assert!(!directory.path().join("model.gguf").exists());
    assert!(!directory.path().join("model.gguf.part").exists());
    Ok(())
}

#[tokio::test]
async fn cached_downloads_require_the_pinned_bytes_and_preserve_a_mismatch() -> anyhow::Result<()> {
    let expected = b"known checkpoint";
    let digest = hex_digest(&Sha256::digest(expected));
    let download = Download {
        url: "https://example.invalid/model.gguf",
        size: expected.len() as u64,
        sha256: &digest,
    };
    let (progress, _) = watch::channel(Progress::default());
    for bytes in [expected.as_slice(), b"stale checkpoint", b"truncated"] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("model.gguf");
        fs::write(&path, bytes)?;
        let result = fetch_into(&download, directory.path(), &progress).await;
        assert_eq!(result.is_ok(), bytes == expected);
        assert_eq!(
            fs::read(&path)?,
            bytes,
            "A checksum failure changed the cached file"
        );
    }
    Ok(())
}
