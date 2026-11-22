use std::path::PathBuf;

use async_channel::Receiver;
use speakeasy_platform::OwnedThread;

use crate::config::{Config, ValidConfig};

pub(super) struct SavedConfig {
    pub(super) submitted: Config,
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
    Finished(SaveOutcome),
}

pub(super) enum SaveOutcome {
    Saved(Box<SavedConfig>),
    Failed {
        error: String,
        earlier: Option<Box<SavedConfig>>,
    },
}

pub(super) struct SaveQueue {
    path: PathBuf,
    writing: Option<Running>,
    held: [Config; 2],
    opened: Option<Config>,
}

impl SaveQueue {
    pub(super) fn new(path: PathBuf, draft: Config) -> Self {
        let held = Config::held_by(&path, &draft);
        Self {
            path,
            writing: None,
            held: [held.clone(), held],
            opened: Some(draft),
        }
    }

    pub(super) fn needs_save(&self, draft: &Config) -> bool {
        match (&self.writing, &self.opened) {
            (Some(running), _) => running.queued.as_ref().unwrap_or(&running.draft) != draft,
            (None, Some(opened)) => opened != draft,
            (None, None) => !self.held.contains(draft),
        }
    }

    pub(super) fn submit(&mut self, draft: Config) -> Option<SaveProgress> {
        if let Some(running) = &mut self.writing {
            running.queued = Some(draft);
            return None;
        }
        Some(self.start(draft, None))
    }

    pub(super) fn finish(&mut self, result: anyhow::Result<SavedConfig>) -> SaveProgress {
        let (queued, earlier) = self
            .writing
            .take()
            .map_or((None, None), |running| (running.queued, running.earlier));
        if let Ok(saved) = &result {
            self.held = saved.held();
            self.opened = None;
        }
        match (queued, result) {
            (Some(draft), Ok(saved)) => self.start(draft, Some(saved)),
            (Some(draft), Err(_)) => self.start(draft, earlier),
            (None, result) => finished(result, earlier),
        }
    }

    fn start(&mut self, draft: Config, earlier: Option<SavedConfig>) -> SaveProgress {
        let (held, path, submitted) = (self.held.clone(), self.path.clone(), draft.clone());
        let (complete, result) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("settings-save", move || {
            let write = || -> anyhow::Result<SavedConfig> {
                let validated = submitted.validated(&path)?;
                Config::save_changes(&held.each_ref(), &submitted, &validated, &path)?;
                Ok(SavedConfig {
                    submitted,
                    validated,
                })
            };
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A closed result receiver needs no completion; the requested write still runs"
            )]
            let _ = complete.try_send(write());
        });
        match thread {
            Ok(thread) => {
                self.writing = Some(Running {
                    _thread: thread,
                    draft,
                    queued: None,
                    earlier,
                });
                SaveProgress::Writing(result)
            },
            Err(error) => finished(Err(error.into()), earlier),
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        self.writing.is_none()
    }
}

fn finished(result: anyhow::Result<SavedConfig>, earlier: Option<SavedConfig>) -> SaveProgress {
    SaveProgress::Finished(match result {
        Ok(saved) => SaveOutcome::Saved(Box::new(saved)),
        Err(error) => SaveOutcome::Failed {
            error: format!("{error:#}"),
            earlier: earlier.map(Box::new),
        },
    })
}

struct Running {
    _thread: OwnedThread,
    draft: Config,
    queued: Option<Config>,
    earlier: Option<SavedConfig>,
}

#[cfg(test)]
mod tests;
