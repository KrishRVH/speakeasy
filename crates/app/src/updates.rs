//! Explicit release discovery. Only a parsed version can become a link to our release page;
//! checking never downloads an executable, installs an update, or runs at startup.

use std::{fmt, time::Duration};

use anyhow::{Context as _, ensure};
use async_channel::Receiver;
use reqwest::Client;
use serde::Deserialize;
use speakeasy_platform::StoppableThread;

use crate::http::{Trust, client_builder};

const RELEASE: &str = "https://api.github.com/repos/KrishRVH/speakeasy/releases/latest";
const FAILED: &str = "Cannot check for updates. Check your connection and try again.";
const MAX_REPLY: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version([u32; 3]);

impl Version {
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut numbers = [0; 3];
        for number in &mut numbers {
            let part = parts.next()?;
            if part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return None;
            }
            *number = part.parse().ok()?;
        }
        parts.next().is_none().then_some(Self(numbers))
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

/// One check owns its cancellable thread until completion or shutdown acknowledges it.
pub(crate) struct Check {
    thread: StoppableThread,
    result: Receiver<anyhow::Result<Option<Version>>>,
}

impl Check {
    pub(crate) fn start() -> anyhow::Result<Self> {
        let (completed, result) = async_channel::bounded(1);
        let thread = StoppableThread::spawn("update-check", move |cancelled| {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    tokio::select! {
                        biased;
                        _ = cancelled.recv() => Ok(None),
                        result = check() => result,
                    }
                })
            })();
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Closing Settings can release the result while the owned check cancels"
            )]
            let _ = completed.try_send(result);
        })?;
        Ok(Self { thread, result })
    }

    pub(crate) fn outcome(&self) -> impl Future<Output = anyhow::Result<Option<Version>>> + use<> {
        let receipt = self.result.clone();
        let stopped = self.thread.stopped();
        async move {
            let result = receipt.recv().await.context(FAILED)?;
            stopped.await;
            result.context(FAILED)
        }
    }

    pub(crate) fn request_stop(&self) {
        self.thread.request_stop();
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.thread.is_finished()
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
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .context(FAILED)
}

async fn fetch(client: &Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let mut response = client.get(url).send().await.context(FAILED)?;
    ensure!(response.status().is_success(), FAILED);
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= MAX_REPLY as u64),
        FAILED
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.context(FAILED)? {
        ensure!(chunk.len() <= MAX_REPLY.saturating_sub(bytes.len()), FAILED);
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
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
