use std::{fmt, time::Duration};

use anyhow::{Context as _, ensure};
use async_channel::Receiver;
use reqwest::Client;
use serde::Deserialize;
use speakeasy_platform::StoppableThread;

use crate::http::{Trust, client_builder};

const RELEASE: &str = "https://api.github.com/repos/KrishRVH/speakeasy/releases/latest";
const FAILED: &str = "Cannot check for updates. Check your connection and try again.";

#[cfg(any(target_os = "windows", test))]
mod install;
#[cfg(target_os = "windows")]
pub(crate) use install::{Download, Installer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version([u32; 3]);

impl Version {
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.').map(str::parse::<u32>);
        let version = Self([
            parts.next()?.ok()?,
            parts.next()?.ok()?,
            parts.next()?.ok()?,
        ]);
        parts.next().is_none().then_some(version)
    }

    pub(crate) fn release_url(self) -> String {
        format!("https://github.com/KrishRVH/speakeasy/releases/tag/v{self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, patch] = self.0;
        write!(formatter, "{major}.{minor}.{patch}")
    }
}

pub(crate) struct Job<T> {
    thread: StoppableThread,
    result: Receiver<anyhow::Result<Option<T>>>,
}

pub(crate) type Check = Job<Version>;

impl<T: Send + 'static> Job<T> {
    fn spawn<F>(
        name: &'static str,
        work: impl FnOnce() -> F + Send + 'static,
    ) -> anyhow::Result<Self>
    where
        F: Future<Output = anyhow::Result<Option<T>>>,
    {
        let (completed, result) = async_channel::bounded(1);
        let thread = StoppableThread::spawn(name, move |cancelled| {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| {
                    runtime.block_on(async {
                        tokio::select! {
                            biased;
                            _ = cancelled.recv() => Ok(None),
                            result = work() => result,
                        }
                    })
                });
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Closing Settings can release the result while the owned update operation cancels"
            )]
            let _ = completed.try_send(result);
        })?;
        Ok(Self { thread, result })
    }

    pub(crate) fn outcome(&self) -> impl Future<Output = anyhow::Result<Option<T>>> + use<T> {
        let receipt = self.result.clone();
        let stopped = self.thread.stopped();
        async move {
            let result = receipt
                .recv()
                .await
                .context("Update work stopped unexpectedly. Try again.")?;
            stopped.await;
            result
        }
    }

    pub(crate) fn request_stop(&self) {
        self.thread.request_stop();
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }
}

impl Check {
    pub(crate) fn start() -> anyhow::Result<Self> {
        Self::spawn("update-check", check)
    }
}

async fn check() -> anyhow::Result<Option<Version>> {
    let client = client(client_builder(Trust::System)?)?;
    let body = fetch(&client, RELEASE).await?;
    newer(&body, env!("CARGO_PKG_VERSION"))
}

fn client(builder: reqwest::ClientBuilder) -> anyhow::Result<Client> {
    builder
        .user_agent("Speakeasy")
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .build()
        .context(FAILED)
}

async fn fetch(client: &Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let response = client.get(url).send().await.context(FAILED)?;
    ensure!(response.status().is_success(), FAILED);
    Ok(response.bytes().await.context(FAILED)?.to_vec())
}

fn newer(body: &[u8], installed: &str) -> anyhow::Result<Option<Version>> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
    }
    let release: Release = serde_json::from_slice(body).context(FAILED)?;
    let version = release
        .tag_name
        .strip_prefix('v')
        .and_then(Version::parse)
        .context(FAILED)?;
    let installed = Version::parse(installed).context(FAILED)?;
    Ok((version > installed).then_some(version))
}

#[cfg(test)]
mod tests;
