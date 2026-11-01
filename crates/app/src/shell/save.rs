use std::path::PathBuf;

use async_channel::Receiver;
use speakeasy_platform::OwnedThread;

use super::lifecycle::ConfigEpoch;
use crate::config::{Config, ValidConfig};

pub(super) struct SavedConfig {
    submitted: Config,
    pub(super) validated: ValidConfig,
}

impl SavedConfig {
    pub(super) fn update_draft(&self, draft: &mut Config) {
        if *draft == self.submitted {
            *draft = Config::clone(&self.validated);
        }
    }

    fn held(&self) -> [Config; 2] {
        [self.submitted.clone(), Config::clone(&self.validated)]
    }
}

pub(super) enum SaveProgress {
    Writing(Receiver<anyhow::Result<SavedConfig>>),
    Finished {
        saved: Option<Box<SavedConfig>>,
        error: Option<String>,
        epoch: ConfigEpoch,
    },
}

pub(super) struct SaveQueue {
    path: PathBuf,
    writing: Option<Running>,
    saved: Option<SavedConfig>,
    durable: [Config; 2],
    opened: Option<Config>,
    requested: Option<Config>,
    epoch: ConfigEpoch,
}

impl SaveQueue {
    pub(super) fn new(path: PathBuf, draft: Config) -> Self {
        let held = Config::held_by(&path, &draft);
        Self {
            path,
            writing: None,
            saved: None,
            durable: [held.clone(), held],
            opened: Some(draft),
            requested: None,
            epoch: ConfigEpoch::default(),
        }
    }

    pub(super) fn needs_save(&self, draft: &Config) -> bool {
        let saved = match &self.opened {
            Some(opened) => opened == draft,
            None => self.durable.contains(draft),
        };
        !saved && self.requested.as_ref() != Some(draft)
    }

    pub(super) fn submit(&mut self, draft: Config, epoch: ConfigEpoch) -> Option<SaveProgress> {
        self.epoch = epoch;
        self.requested = Some(draft.clone());
        if let Some(running) = &mut self.writing {
            running.queued = Some(draft);
            return None;
        }
        Some(self.start(draft))
    }

    pub(super) fn finish(&mut self, result: anyhow::Result<SavedConfig>) -> SaveProgress {
        let queued = self.writing.take().and_then(|running| running.queued);
        let error = match result {
            Ok(saved) => {
                self.durable = saved.held();
                self.opened = None;
                self.saved = Some(saved);
                None
            },
            Err(error) => Some(format!("{error:#}")),
        };
        if let Some(next) = queued {
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
            Ok(work) => {
                let running = self.writing.insert(Running { work, queued: None });
                SaveProgress::Writing(running.work.result.clone())
            },
            Err(error) => self.finish(Err(error)),
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        self.writing.is_none()
    }

    pub(super) fn flush(&mut self) {
        let Some(Running {
            work: SaveWork { thread, result },
            queued,
        }) = self.writing.take()
        else {
            return;
        };
        drop(thread);

        if let Ok(Ok(saved)) = result.try_recv() {
            self.durable = saved.held();
        }
        if let Some(queued) = queued {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Dropping the work joins its thread; a closing view has no one to tell of a failed start"
            )]
            let _ = SaveWork::start(self.durable.clone(), queued, self.path.clone());
        }
    }
}

struct Running {
    work: SaveWork,
    queued: Option<Config>,
}

struct SaveWork {
    thread: OwnedThread,
    result: Receiver<anyhow::Result<SavedConfig>>,
}

impl SaveWork {
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
