use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use super::*;
use crate::{
    http::{Trust, client_builder},
    setup::CHECKING,
};

const BODY: &[u8] = b"Say the word. The door opens, and what is said stays inside.";

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
    let (progress, _) = watch::channel(CHECKING);
    let path = fetch_into(&download, directory.path(), &progress).await?;
    assert_eq!(requests.recv().await?, Some(20));
    assert_eq!(fs::read(&path)?, BODY);
    assert!(!directory.path().join("model.gguf.part").exists());
    assert!(matches!(
        *progress.borrow(),
        Progress::Bytes { done, .. } if done == BODY.len() as u64
    ));
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
    let (progress, _) = watch::channel(CHECKING);
    let error = fetch_into(&download, directory.path(), &progress)
        .await
        .err()
        .context("A download that failed its checksum was kept")?;
    assert!(format!("{error:#}").contains("checksum"), "{error:#}");
    assert!(!directory.path().join("model.gguf").exists());
    assert!(!directory.path().join("model.gguf.part").exists());
    Ok(())
}
