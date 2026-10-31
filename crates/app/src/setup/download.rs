//! Pinned downloads: each file is fetched by URL, checked against its published size and SHA-256,
//! resumed from its partial file after an interruption, and renamed into place only once verified.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, bail};
use reqwest::{Client, StatusCode, header::RANGE};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncReadExt, sync::watch};

use super::{Progress, file_error};

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
        let size = fs::metadata(&path).map_err(file_error(directory))?.len();
        if size != download.size
            || hex_digest(
                &hash_partial(&path, size, progress)
                    .await
                    .map_err(file_error(directory))?
                    .finalize(),
            ) != download.sha256
        {
            bail!(
                "The installed download at {} does not match its published checksum. Move this file aside and try setup again.",
                path.display()
            );
        }
        return Ok(path);
    }
    fs::create_dir_all(directory).map_err(file_error(directory))?;
    let partial = directory.join(format!("{name}.part"));
    let mut done = fs::metadata(&partial).map_or(0, |metadata| metadata.len());
    if done == download.size {
        let hasher = hash_partial(&partial, done, progress)
            .await
            .map_err(file_error(directory))?;
        if hex_digest(&hasher.finalize()) == download.sha256 {
            fs::rename(&partial, &path).map_err(file_error(directory))?;
            return Ok(path);
        }
        fs::remove_file(&partial).map_err(file_error(directory))?;
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
        .context("Cannot download. Check your internet connection, then try again.")?;
    let status = response.status();
    if !status.is_success() {
        bail!("The download server answered {status}. Try again later.");
    }
    let (mut hasher, mut file) = if done > 0 && status == StatusCode::PARTIAL_CONTENT {
        (
            hash_partial(&partial, done, progress)
                .await
                .map_err(file_error(directory))?,
            OpenOptions::new()
                .append(true)
                .open(&partial)
                .map_err(file_error(directory))?,
        )
    } else {
        done = 0;
        (
            Sha256::new(),
            File::create(&partial).map_err(file_error(directory))?,
        )
    };
    progress.send_replace(Progress {
        step,
        done,
        total: download.size,
    });
    while let Some(chunk) = response
        .chunk()
        .await
        .context("The download stopped. Check your internet connection, then try again.")?
    {
        done = done.saturating_add(chunk.len() as u64);
        if done > download.size {
            break;
        }
        hasher.update(&chunk);
        file.write_all(&chunk).map_err(file_error(directory))?;
        progress.send_modify(|progress| progress.done = done);
    }
    file.sync_all().map_err(file_error(directory))?;
    drop(file);
    if done != download.size || hex_digest(&hasher.finalize()) != download.sha256 {
        fs::remove_file(&partial).map_err(file_error(directory))?;
        bail!("A download did not match its published checksum. Try again.");
    }
    fs::rename(&partial, &path).map_err(file_error(directory))?;
    Ok(path)
}

pub(super) fn hex_digest(bytes: &[u8]) -> String {
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
) -> io::Result<Sha256> {
    progress.send_replace(Progress {
        step: "Verifying downloaded data",
        done: 0,
        total: size,
    });
    let mut file = tokio::fs::File::open(partial).await?;
    // Every asynchronous read is a blocking-pool round trip, so read a MiB at a time.
    let mut chunk = Vec::with_capacity(1024 * 1024);
    let mut hasher = Sha256::new();
    loop {
        chunk.clear();
        let count = file.read_buf(&mut chunk).await?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk);
        progress.send_modify(|progress| {
            progress.done = progress.done.saturating_add(count as u64);
        });
        // Cached reads can complete without yielding; let cancellation run between chunks.
        tokio::task::yield_now().await;
    }
    Ok(hasher)
}

pub(super) fn file_name(url: &str) -> &str {
    url.rsplit_once('/').map_or(url, |(_, name)| name)
}

#[cfg(test)]
mod tests;
