use std::path::PathBuf;

use async_channel::Receiver;
use speakeasy_core::lifecycle::ConfigEpoch;
use speakeasy_platform::OwnedThread;

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
        outcome: SaveOutcome,
        epoch: ConfigEpoch,
    },
}

pub(super) enum SaveOutcome {
    Saved(Box<SavedConfig>),
    Failed {
        error: String,
        kept: Option<Box<SavedConfig>>,
    },
}

pub(super) struct SaveQueue {
    path: PathBuf,
    writing: Option<Running>,
    saved: Option<SavedConfig>,
    durable: [Config; 2],
    opened: Option<Config>,
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
            epoch: ConfigEpoch::default(),
        }
    }

    pub(super) fn needs_save(&self, draft: &Config) -> bool {
        match (&self.writing, &self.opened) {
            (Some(running), _) => running.queued.as_ref().unwrap_or(&running.draft) != draft,
            (None, Some(opened)) => opened != draft,
            (None, None) => !self.durable.contains(draft),
        }
    }

    pub(super) fn submit(&mut self, draft: Config, epoch: ConfigEpoch) -> Option<SaveProgress> {
        self.epoch = epoch;
        if let Some(running) = &mut self.writing {
            running.queued = Some(draft);
            return None;
        }
        Some(self.start(draft))
    }

    pub(super) fn finish(&mut self, result: anyhow::Result<SavedConfig>) -> SaveProgress {
        let queued = self.writing.take().and_then(|running| running.queued);
        if let Ok(saved) = &result {
            self.durable = saved.held();
            self.opened = None;
        }
        if let Some(next) = queued {
            if let Ok(saved) = result {
                self.saved = Some(saved);
            }
            return self.start(next);
        }
        let kept = self.saved.take();
        let outcome = match result {
            Ok(saved) => SaveOutcome::Saved(Box::new(saved)),
            Err(error) => SaveOutcome::Failed {
                error: format!("{error:#}"),
                kept: kept.map(Box::new),
            },
        };
        SaveProgress::Finished {
            outcome,
            epoch: self.epoch,
        }
    }

    fn start(&mut self, draft: Config) -> SaveProgress {
        match SaveWork::start(self.durable.clone(), draft.clone(), self.path.clone()) {
            Ok(work) => {
                let running = self.writing.insert(Running {
                    work,
                    draft,
                    queued: None,
                });
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
            ..
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
    draft: Config,
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
