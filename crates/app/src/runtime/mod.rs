//! Thread ownership and presentation interface for the dictation owner.
//! Stop revokes authority immediately; stopped acknowledges owned native cleanup.
mod microphone;
mod owner;
mod session;
mod worker;
use crate::{
    config::Config,
    ports::{Desktop, Ports, Recording, Speech},
};
use async_channel::Receiver;
use owner::run;
pub(crate) use session::SessionId;
use speakeasy_platform::Input;
#[cfg(test)]
use std::time::Duration;
use std::{thread, time::Instant};
use tokio::sync::watch;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Idle,
    Starting,
    Recording,
    Stopping,
    Processing,
    Done,
    Empty,
    Cancelled,
    Error,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ModelState {
    Loading,
    Ready,
    #[default]
    Unavailable,
}

#[derive(Clone, PartialEq)]
pub(crate) struct Snapshot {
    pub epoch: u64,
    pub id: u64,
    pub phase: Phase,
    pub hands_free: bool,
    pub level: f32,
    pub meter_tick: u64,
    pub started: Instant,
    pub message: String,
    pub model: ModelState,
    pub desktop_ready: bool,
    pub shortcut: std::sync::Arc<str>,
    pub cancel_shortcut: std::sync::Arc<str>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            epoch: 0,
            id: 0,
            phase: Phase::Idle,
            hands_free: false,
            level: 0.0,
            meter_tick: 0,
            started: tokio::time::Instant::now().into_std(),
            message: String::new(),
            model: ModelState::Unavailable,
            desktop_ready: true,
            shortcut: speakeasy_platform::SHORTCUT.into(),
            cancel_shortcut: if cfg!(target_os = "linux") {
                "Ctrl + Super + Escape"
            } else {
                "Escape"
            }
            .into(),
        }
    }
}

impl Snapshot {
    fn startup_failed(&mut self, epoch: u64, error: &impl std::fmt::Display) {
        if self.epoch != epoch {
            return;
        }
        self.phase = Phase::Error;
        self.message = format!("Could not start dictation worker: {error}");
    }
}

// The loading notice clears once the first model is ready, unless replaced.
pub(crate) const LOADING: &str = "Loading local model…";

pub(crate) enum Event {
    Ready(SessionId),
    Level(SessionId, f32),
    AudioDone(SessionId, anyhow::Result<Option<Vec<u8>>>),
}

#[cfg(test)]
mod fixture;

pub(crate) struct Runtime {
    pub input: speakeasy_platform::InputSender,
    thread: Option<thread::JoinHandle<()>>,
    configuration: watch::Sender<Config>,
    stop: async_channel::Sender<()>,
    finished: async_channel::Receiver<()>,
}
impl Runtime {
    pub(crate) fn start(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
    ) -> anyhow::Result<(Self, speakeasy_platform::InputMonitor)> {
        let (sender, receiver) = async_channel::bounded(64);
        let input = speakeasy_platform::InputSender::new(sender);
        let (monitor, inserter) =
            speakeasy_platform::prepare(input.clone(), config.desktop_options())?;
        let runtime = Self::spawn(config, snapshots, Desktop { inserter }, input, receiver)?;
        Ok((runtime, monitor))
    }
    #[cfg(test)]
    fn start_with<P: Ports>(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
        ports: P,
    ) -> anyhow::Result<Self> {
        let (input, receiver) = async_channel::bounded(64);
        let input = speakeasy_platform::InputSender::new(input);
        Self::spawn(config, snapshots, ports, input, receiver)
    }
    fn spawn<P: Ports>(
        config: Config,
        snapshots: watch::Sender<Snapshot>,
        ports: P,
        input: speakeasy_platform::InputSender,
        receiver: Receiver<Input>,
    ) -> anyhow::Result<Self> {
        let epoch = snapshots.borrow().epoch;
        let gate = input.clone();
        let (configuration, changes) = watch::channel(config);
        let (stop, stopping) = async_channel::bounded(1);
        let (finished_tx, finished) = async_channel::bounded(1);
        let thread = thread::Builder::new()
            .name("dictation".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        runtime.block_on(run(
                            changes, receiver, snapshots, gate, stopping, ports, epoch,
                        ));
                    },
                    Err(error) => snapshots.send_modify(|s| s.startup_failed(epoch, &error)),
                }
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Closing the one-shot receiver means the UI owner no longer waits for runtime retirement"
                )]
                let _ = finished_tx.try_send(());
            })?;
        Ok(Self {
            input,
            thread: Some(thread),
            configuration,
            stop,
            finished,
        })
    }
    pub(crate) fn is_running(&self) -> bool {
        !self.input.is_closed()
            && self
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
    }
    pub(crate) fn configure(&self, config: Config) {
        self.input.cancel();
        self.configuration.send_replace(config);
    }
    pub(crate) fn request_stop(&self) {
        // Wake the owner through a separate lane before closing input, so a
        // normal pause never reports shortcut failure or drains queued presses.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A full stop lane already requests shutdown; a closed lane belongs to an exited owner"
        )]
        let _ = self.stop.try_send(());
        self.input.close();
    }
    pub(crate) fn stopped(&self) -> impl std::future::Future<Output = ()> + use<> {
        let finished = self.finished.clone();
        async move {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Completion or sender closure after a runtime panic both permit joining its owned thread"
            )]
            let _ = finished.recv().await;
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Joining reaps the owner even after panic; drop cannot publish a new session error"
            )]
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests;
