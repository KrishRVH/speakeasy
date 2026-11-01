use std::{
    fs::{File, OpenOptions},
    io::{Read as _, Write as _},
    path::Path,
    time::Duration,
};

use anyhow::{Context as _, ensure};
use reqwest::{Client, Url};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio::sync::watch;

#[cfg(target_os = "windows")]
use super::fetch;
use super::{Job, Version};
use crate::http::{Trust, client_builder};

mod cache;
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

#[derive(Clone, Copy)]
pub(crate) struct Progress {
    pub(crate) stage: &'static str,
    pub(crate) done: u64,
    pub(crate) total: u64,
}

#[cfg(target_os = "windows")]
pub(crate) type Download = Job<Installer>;

struct Payload {
    file: File,
    directory: TempDir,
    version: Version,
    _lease: File,
}

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
        let installed = Version::parse(env!("CARGO_PKG_VERSION")).context(INVALID_FILE)?;
        ensure!(
            version > installed,
            "This release is not newer than the installed version."
        );
        let root = crate::paths::data_root()?.join("updates");
        let (progress, observed) = watch::channel(Progress {
            stage: "Authenticating update",
            done: 0,
            total: 0,
        });
        let operation = Self::spawn("update-download", move || async move {
            let target = handoff::Target::current(configuration)?;
            let client = client(client_builder(Trust::System)?.https_only(true))?;
            let metadata = fetch(
                &client,
                &asset_url(version, "update.json"),
                manifest::MAX_MANIFEST,
            )
            .await
            .context(DOWNLOAD_FAILED)?;
            let signature = fetch(&client, &asset_url(version, "update.json.sig"), 64)
                .await
                .context(DOWNLOAD_FAILED)?;
            let artifact = manifest::verify(&metadata, &signature, &key, version, installed)?;
            let payload = download(
                &client,
                &asset_url(artifact.version, manifest::NAME),
                &artifact,
                &root,
                &progress,
            )
            .await?;
            Ok(Some(Installer::new(payload, target)))
        })?;
        Ok((operation, observed))
    }
}

fn asset_url(version: Version, name: &str) -> String {
    format!("https://github.com/KrishRVH/speakeasy/releases/download/v{version}/{name}")
}

fn client(builder: reqwest::ClientBuilder) -> anyhow::Result<Client> {
    builder
        .user_agent("Speakeasy")
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 4 && trusted_redirect(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("Update download redirected outside its release hosts")
            }
        }))
        .build()
        .context(DOWNLOAD_FAILED)
}

fn trusted_redirect(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

async fn download(
    client: &Client,
    url: &str,
    artifact: &manifest::Artifact,
    root: &Path,
    progress: &watch::Sender<Progress>,
) -> anyhow::Result<Payload> {
    let lease = cache::acquire(root)?;
    let directory = tempfile::Builder::new()
        .prefix("release-")
        .tempdir_in(root)
        .context(DOWNLOAD_FAILED)?;
    let path = directory.path().join(manifest::NAME);
    let mut file = File::create_new(&path).context(DOWNLOAD_FAILED)?;
    let mut response = client.get(url).send().await.context(DOWNLOAD_FAILED)?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        DOWNLOAD_FAILED
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size == artifact.size),
        INVALID_FILE
    );
    let mut done = 0_u64;
    progress.send_replace(Progress {
        stage: "Downloading update",
        done,
        total: artifact.size,
    });
    while let Some(chunk) = response.chunk().await.context(DOWNLOAD_FAILED)? {
        let length = u64::try_from(chunk.len()).context(INVALID_FILE)?;
        ensure!(length <= artifact.size.saturating_sub(done), INVALID_FILE);
        file.write_all(&chunk).context(DOWNLOAD_FAILED)?;
        done = done.saturating_add(length);
        progress.send_modify(|progress| progress.done = done);
        tokio::task::yield_now().await;
    }
    ensure!(done == artifact.size, INVALID_FILE);
    file.sync_all().context(DOWNLOAD_FAILED)?;
    drop(file);
    let mut file = open_verified(&path)?;
    let metadata = file.metadata().context(DOWNLOAD_FAILED)?;
    ensure!(
        metadata.is_file() && metadata.len() == artifact.size,
        INVALID_FILE
    );
    progress.send_replace(Progress {
        stage: "Verifying update",
        done: 0,
        total: artifact.size,
    });
    let mut hash = Sha256::new();
    let mut hashed = 0_u64;
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).context(DOWNLOAD_FAILED)?;
        if read == 0 {
            break;
        }
        let length = u64::try_from(read).context(INVALID_FILE)?;
        hashed = hashed
            .checked_add(length)
            .filter(|&total| total <= artifact.size)
            .context(INVALID_FILE)?;
        hash.update(buffer.get(..read).context(INVALID_FILE)?);
        progress.send_modify(|progress| progress.done = hashed);
        tokio::task::yield_now().await;
    }
    ensure!(
        <[u8; 32]>::from(hash.finalize()) == artifact.sha256,
        INVALID_FILE
    );
    Ok(Payload {
        file,
        directory,
        version: artifact.version,
        _lease: lease,
    })
}

fn open_verified(path: &Path) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.share_mode(1);
    }
    options.open(path).context(DOWNLOAD_FAILED)
}
