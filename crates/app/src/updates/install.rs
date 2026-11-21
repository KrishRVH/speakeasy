use std::{
    fs::{self, File},
    io::Write as _,
    path::Path,
};

use anyhow::{Context as _, ensure};
use reqwest::Client;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio::sync::watch;

use super::Job;
#[cfg(target_os = "windows")]
use super::{Version, client, fetch};
use crate::progress::Progress;

#[cfg(target_os = "windows")]
mod handoff;
mod manifest;
#[cfg(target_os = "windows")]
pub(crate) use handoff::Installer;

#[cfg(test)]
mod tests;

const DOWNLOAD_FAILED: &str =
    "Cannot download the update. Check your connection and free disk space, then try again.";
const INVALID_FILE: &str = "The downloaded installer does not match the authenticated release. Nothing was installed. Try again.";

#[cfg(target_os = "windows")]
pub(crate) type Download = Job<Installer>;

#[cfg(target_os = "windows")]
impl Download {
    pub(crate) fn available() -> bool {
        manifest::key().is_ok()
    }

    pub(crate) fn prepare(
        version: Version,
        configuration: std::path::PathBuf,
    ) -> anyhow::Result<(Self, watch::Receiver<Progress>)> {
        let key = manifest::key()?;
        let root = crate::paths::data_root()?;
        let (progress, observed) = watch::channel(Progress::Step("Authenticating update"));
        let operation = Self::spawn("update-download", move || async move {
            let target = handoff::Target::current(configuration)?;
            let client = client(crate::http::client_builder(crate::http::Trust::System)?)?;
            let metadata = fetch(&client, &asset_url(version, "update.json"))
                .await
                .context(DOWNLOAD_FAILED)?;
            let signature = fetch(&client, &asset_url(version, "update.json.sig"))
                .await
                .context(DOWNLOAD_FAILED)?;
            let artifact = manifest::verify(&metadata, &signature, &key)?;
            let url = asset_url(version, manifest::NAME);
            let staged = download(&client, &url, &artifact, &root, &progress).await?;
            Ok(Some(Installer::new(staged, version, target)))
        })?;
        Ok((operation, observed))
    }
}

#[cfg(target_os = "windows")]
fn asset_url(version: Version, name: &str) -> String {
    format!("https://github.com/KrishRVH/speakeasy/releases/download/v{version}/{name}")
}

async fn download(
    client: &Client,
    url: &str,
    artifact: &manifest::Artifact,
    root: &Path,
    progress: &watch::Sender<Progress>,
) -> anyhow::Result<TempDir> {
    let cache = root.join("updates");
    #[expect(
        clippy::let_underscore_must_use,
        reason = "An installer still running from an earlier update keeps its copy; the next update clears it"
    )]
    let _ = fs::remove_dir_all(&cache);
    fs::create_dir_all(&cache).context(DOWNLOAD_FAILED)?;
    let directory = tempfile::Builder::new()
        .prefix("release-")
        .tempdir_in(&cache)
        .context(DOWNLOAD_FAILED)?;
    let mut file =
        File::create_new(directory.path().join(manifest::NAME)).context(DOWNLOAD_FAILED)?;
    let mut response = client.get(url).send().await.context(DOWNLOAD_FAILED)?;
    ensure!(response.status().is_success(), DOWNLOAD_FAILED);
    let step = "Downloading update";
    let mut hash = Sha256::new();
    let mut done = 0_u64;
    progress.send_replace(Progress::bytes(step, done, artifact.size));
    while let Some(chunk) = response.chunk().await.context(DOWNLOAD_FAILED)? {
        done = done.saturating_add(chunk.len() as u64);
        ensure!(done <= artifact.size, INVALID_FILE);
        hash.update(&chunk);
        file.write_all(&chunk).context(DOWNLOAD_FAILED)?;
        progress.send_replace(Progress::bytes(step, done, artifact.size));
        tokio::task::yield_now().await;
    }
    ensure!(
        <[u8; 32]>::from(hash.finalize()) == artifact.sha256,
        INVALID_FILE
    );
    Ok(directory)
}
