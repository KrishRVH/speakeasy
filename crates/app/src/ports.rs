//! The three side effects used by the session owner. Tests replace devices and
//! speech at this boundary; gesture, cancellation, and insertion ordering stay real.
use crate::{audio::Capture, config::Config, local_speech::LocalSpeech, runtime::Event};
use speakeasy_platform::{InsertPermit, Inserted, Inserter};
use std::future::Future;
use tokio::sync::watch;

pub trait Recording: Send + 'static {
    fn finish(&self);
    fn retire(self) -> impl Future<Output = ()> + Send + 'static;
}
impl Recording for Capture {
    fn finish(&self) {
        Capture::finish(self);
    }
    fn retire(self) -> impl Future<Output = ()> + Send + 'static {
        Capture::retire(self)
    }
}
pub trait Speech: Send + Sync + 'static {
    fn transcribe(
        &self,
        wav: Vec<u8>,
        language: &str,
    ) -> impl Future<Output = anyhow::Result<String>> + Send;
    fn idle(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn stop(&mut self) -> impl Future<Output = ()> + Send;
}
impl Speech for LocalSpeech {
    async fn transcribe(&self, wav: Vec<u8>, language: &str) -> anyhow::Result<String> {
        LocalSpeech::transcribe(self, wav, language).await
    }
    async fn idle(&self) -> anyhow::Result<()> {
        LocalSpeech::idle(self).await
    }
    async fn stop(&mut self) {
        LocalSpeech::stop(self).await;
    }
}
pub trait Ports: Send + 'static {
    fn prepares_desktop(&self) -> bool {
        false
    }
    type Recording: Recording;
    type Speech: Speech;
    fn record(
        &self,
        id: u64,
        microphone: Option<String>,
        events: async_channel::Sender<Event>,
    ) -> anyhow::Result<Self::Recording>;
    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<Self::Speech>> + Send + 'static;
    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        preserve: bool,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static;
}
pub struct Desktop {
    pub inserter: Inserter,
}
impl Ports for Desktop {
    fn prepares_desktop(&self) -> bool {
        true
    }
    type Recording = Capture;
    type Speech = LocalSpeech;
    fn record(
        &self,
        id: u64,
        microphone: Option<String>,
        events: async_channel::Sender<Event>,
    ) -> anyhow::Result<Capture> {
        Capture::start(id, microphone, events)
    }
    fn load(
        &self,
        config: Config,
        cancelled: watch::Receiver<bool>,
    ) -> impl Future<Output = anyhow::Result<LocalSpeech>> + Send + 'static {
        LocalSpeech::start(config, cancelled)
    }
    fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        preserve: bool,
    ) -> impl Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let inserter = self.inserter.clone();
        async move { inserter.insert(text, permit, preserve).await }
    }
}
