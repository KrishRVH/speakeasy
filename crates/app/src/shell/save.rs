//! Durable settings writes. A short-lived worker thread owns each write; the queue keeps only the
//! latest request behind it and the last durable result until it drains. Writes run one at a time,
//! so each patches exactly the keys its draft changed since the last write that succeeded.

use std::path::PathBuf;

use async_channel::Receiver;
use speakeasy_platform::OwnedThread;

use super::lifecycle::ConfigEpoch;
use crate::config::{Config, ValidConfig};

/// A durable write: the draft as submitted and the settings validation resolved from it.
pub(super) struct SavedConfig {
    submitted: Config,
    pub(super) validated: ValidConfig,
}

impl SavedConfig {
    /// Adopts the validated settings if the draft is unchanged since submission.
    pub(super) fn update_draft(&self, draft: &mut Config) {
        if *draft == self.submitted {
            *draft = Config::clone(&self.validated);
        }
    }

    /// The settings the file holds after this write, as Settings submitted them and as validation
    /// resolved their paths; a later draft may still have either spelling.
    fn held(&self) -> [Config; 2] {
        [self.submitted.clone(), Config::clone(&self.validated)]
    }
}

pub(super) enum SaveProgress {
    /// A write started; hand its result to [`SaveQueue::finish`].
    Writing(Receiver<anyhow::Result<SavedConfig>>),
    /// The queue drained: the newest durable save, the last write's error, and the newest
    /// request's epoch.
    Finished {
        saved: Option<Box<SavedConfig>>,
        error: Option<String>,
        epoch: ConfigEpoch,
    },
}

pub(super) struct SaveQueue {
    /// The settings file every write patches.
    path: PathBuf,
    writing: Option<SaveWork>,
    /// The draft queued behind the running write.
    pending: Option<Config>,
    saved: Option<SavedConfig>,
    /// The settings the file holds as of the last write that succeeded, in each spelling a draft
    /// may have of them, which the next write compares its draft against.
    durable: [Config; 2],
    /// The draft Settings opened on, which alone has nothing to save until a write succeeds: a
    /// missing file holds only the defaults its keys read as, which a first launch's draft is not.
    opened: Option<Config>,
    /// The newest request's draft until the queue drains.
    requested: Option<Config>,
    /// The newest request's epoch, which the drained queue reports.
    epoch: ConfigEpoch,
}

impl SaveQueue {
    /// A queue that writes the file at `path`, for Settings opened on `draft` from it.
    pub(super) fn new(path: PathBuf, draft: Config) -> Self {
        let held = Config::held_by(&path, &draft);
        Self {
            path,
            writing: None,
            pending: None,
            saved: None,
            durable: [held.clone(), held],
            opened: Some(draft),
            requested: None,
            epoch: ConfigEpoch::default(),
        }
    }

    /// Whether `draft` has edits to save: it is not the draft Settings opened on or, once a write
    /// succeeds, what the file holds in either spelling, and no request in the queue is saving it.
    pub(super) fn needs_save(&self, draft: &Config) -> bool {
        let saved = match &self.opened {
            Some(opened) => opened == draft,
            None => self.durable.contains(draft),
        };
        !saved && self.requested.as_ref() != Some(draft)
    }

    /// Starts writing `draft`, requested at `epoch`, or returns `None` after queuing it behind the
    /// running write in place of any draft queued before.
    pub(super) fn submit(&mut self, draft: Config, epoch: ConfigEpoch) -> Option<SaveProgress> {
        self.epoch = epoch;
        self.requested = Some(draft.clone());
        if self.writing.is_some() {
            self.pending = Some(draft);
            None
        } else {
            Some(self.start(draft))
        }
    }

    /// Records the running write's result, then starts the queued request or reports the drained
    /// queue.
    pub(super) fn finish(&mut self, result: anyhow::Result<SavedConfig>) -> SaveProgress {
        // The result follows the durable write, so this join never waits on storage.
        self.writing = None;
        let error = match result {
            Ok(saved) => {
                self.durable = saved.held();
                self.opened = None;
                self.saved = Some(saved);
                None
            },
            Err(error) => Some(format!("{error:#}")),
        };
        if let Some(next) = self.pending.take() {
            return self.start(next);
        }
        self.requested = None;
        SaveProgress::Finished {
            saved: self.saved.take().map(Box::new),
            error,
            epoch: self.epoch,
        }
    }

    fn start(&mut self, draft: Config) -> SaveProgress {
        match SaveWork::start(self.durable.clone(), draft, self.path.clone()) {
            Ok(work) => SaveProgress::Writing(self.writing.insert(work).result.clone()),
            Err(error) => self.finish(Err(error)),
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        self.writing.is_none() && self.pending.is_none()
    }

    /// Blocks until the running write finishes, then writes the latest queued request and waits for
    /// that write too.
    pub(super) fn flush(&mut self) {
        if let Some(SaveWork { thread, result }) = self.writing.take() {
            drop(thread);
            // The queued write compares against this one, so a revert queued behind it still lands.
            if let Ok(Ok(saved)) = result.try_recv() {
                self.durable = saved.held();
            }
        }
        if let Some(pending) = self.pending.take() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Dropping the work joins its thread; a closing view has no one to tell of a failed start"
            )]
            let _ = SaveWork::start(self.durable.clone(), pending, self.path.clone());
        }
    }
}

/// Validates and writes settings on its own thread; dropping it joins that thread.
struct SaveWork {
    thread: OwnedThread,
    result: Receiver<anyhow::Result<SavedConfig>>,
}

impl SaveWork {
    /// Writes what `draft` changed from `held`, the settings the file holds.
    fn start(held: [Config; 2], draft: Config, path: PathBuf) -> anyhow::Result<Self> {
        Self::spawn(move || {
            let validated = draft.validated(&path)?;
            Config::save_changes(&held.each_ref(), &draft, &validated, &path)?;
            Ok(SavedConfig {
                submitted: draft,
                validated,
            })
        })
    }

    fn spawn(
        write: impl FnOnce() -> anyhow::Result<SavedConfig> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (complete, result) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("settings-save", move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A closed result receiver needs no completion; the requested durable write still runs"
            )]
            let _ = complete.try_send(write());
        })?;
        Ok(Self { thread, result })
    }
}

#[cfg(test)]
mod tests;
