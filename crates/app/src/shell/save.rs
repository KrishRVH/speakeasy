//! Durable settings writes. A short-lived worker thread owns each write; the queue keeps only the
//! latest request behind it and the last durable result until it drains. Writes run one at a time,
//! so each patches exactly the keys its draft changed since the last write that succeeded.

use std::path::PathBuf;

use async_channel::Receiver;
use speakeasy_platform::OwnedThread;

use super::lifecycle::ConfigEpoch;
use crate::config::{Config, ValidConfig};

pub(super) struct SaveRequest {
    pub(super) draft: Config,
    pub(super) epoch: ConfigEpoch,
    pub(super) path: PathBuf,
}

/// A durable write: the draft as submitted and the settings validation resolved from it.
pub(super) struct SavedConfig {
    submitted: Config,
    pub(super) validated: ValidConfig,
}

impl SavedConfig {
    /// Adopts the validated settings if the draft is unchanged since submission, returning whether
    /// it was.
    pub(super) fn update_draft(&self, draft: &mut Config) -> bool {
        if *draft == self.submitted {
            *draft = Config::clone(&self.validated);
            true
        } else {
            false
        }
    }
}

pub(super) enum SaveProgress {
    /// A write started; hand its result to [`SaveQueue::finish`].
    Writing(Receiver<anyhow::Result<SavedConfig>>),
    /// The queue drained: the newest durable save, the latest error, and the newest request's
    /// epoch.
    Finished {
        saved: Option<Box<SavedConfig>>,
        error: Option<String>,
        epoch: ConfigEpoch,
    },
}

pub(super) struct SaveQueue {
    writing: Option<SaveWork>,
    pending: Option<SaveRequest>,
    saved: Option<SavedConfig>,
    /// The settings the file holds as of the last write that succeeded, which the next write
    /// compares its draft against.
    durable: Config,
    /// The newest request's epoch, which the drained queue reports.
    epoch: ConfigEpoch,
}

impl SaveQueue {
    /// A queue for settings that the file holds as `durable`.
    pub(super) fn new(durable: Config) -> Self {
        Self {
            writing: None,
            pending: None,
            saved: None,
            durable,
            epoch: ConfigEpoch::default(),
        }
    }

    /// Starts writing `request`, or returns `None` after queuing it behind the running write in
    /// place of any request queued before.
    pub(super) fn submit(&mut self, request: SaveRequest) -> Option<SaveProgress> {
        self.epoch = request.epoch;
        if self.writing.is_some() {
            self.pending = Some(request);
            None
        } else {
            Some(self.start(request))
        }
    }

    /// Records the running write's result, then starts the queued request or reports the drained
    /// queue.
    pub(super) fn finish(&mut self, result: anyhow::Result<SavedConfig>) -> SaveProgress {
        // The result follows the durable write, so this join never waits on storage.
        self.writing = None;
        let error = match result {
            Ok(saved) => {
                self.durable = Config::clone(&saved.validated);
                self.saved = Some(saved);
                None
            },
            Err(error) => Some(format!("{error:#}")),
        };
        match self.pending.take() {
            Some(next) => self.start(next),
            None => SaveProgress::Finished {
                saved: self.saved.take().map(Box::new),
                error,
                epoch: self.epoch,
            },
        }
    }

    fn start(&mut self, request: SaveRequest) -> SaveProgress {
        match SaveWork::start(self.durable.clone(), request.draft, request.path) {
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
                self.durable = saved.validated.into_inner();
            }
        }
        if let Some(pending) = self.pending.take() {
            // Dropping the work joins its thread; nothing remains to report a failed start to.
            drop(SaveWork::start(
                self.durable.clone(),
                pending.draft,
                pending.path,
            ));
        }
    }
}

/// Validates and writes settings on its own thread; dropping it joins that thread.
struct SaveWork {
    thread: OwnedThread,
    result: Receiver<anyhow::Result<SavedConfig>>,
}

impl SaveWork {
    /// Writes what `draft` changed from `base`, the settings the file holds.
    fn start(base: Config, draft: Config, path: PathBuf) -> anyhow::Result<Self> {
        Self::spawn(move || {
            let validated = draft.validated(&path)?;
            Config::save_changes(&base, &draft, &validated, &path)?;
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
mod tests {
    use std::{sync::mpsc, time::Duration};

    use anyhow::{Context as _, bail};
    use tokio::time::timeout;

    use super::*;
    use crate::{config::valid_config, theme::Theme};

    fn durable(config: Config) -> SavedConfig {
        SavedConfig {
            submitted: config.clone(),
            validated: ValidConfig::assume(config),
        }
    }

    /// Waits for the result of the write `progress` started, for `SaveQueue::finish`.
    async fn write_result(progress: SaveProgress) -> anyhow::Result<anyhow::Result<SavedConfig>> {
        let SaveProgress::Writing(result) = progress else {
            bail!("No write started");
        };
        Ok(timeout(Duration::from_secs(5), result.recv()).await??)
    }

    #[tokio::test]
    async fn queued_save_applies_only_latest_successful_draft() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let config = valid_config(directory.path())?;
        let epoch = ConfigEpoch::default();
        let request = |threads| SaveRequest {
            draft: Config {
                threads,
                ..config.clone()
            },
            epoch,
            path: path.clone(),
        };
        let mut queue = SaveQueue::new(config.clone());
        let first = queue
            .submit(request(4))
            .context("The first save was queued")?;
        assert!(
            queue.submit(request(8)).is_none(),
            "A save started while another was writing"
        );
        assert!(queue.submit(request(16)).is_none());
        let latest = queue.finish(write_result(first).await?);
        let SaveProgress::Finished {
            saved,
            error,
            epoch: finished,
        } = queue.finish(write_result(latest).await?)
        else {
            bail!("Latest save did not finish");
        };
        let saved = saved.context("Successful save was lost")?;
        assert_eq!(
            saved.validated.threads, 16,
            "Repeated Save replaces the queued draft"
        );
        assert_eq!(Config::read(&path)?.threads, 16);
        assert_eq!(finished, epoch);
        assert!(error.is_none());
        assert!(queue.is_idle());
        assert!(queue.saved.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn queued_failure_retains_previous_durable_save_and_reports_latest_error()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let first_epoch = ConfigEpoch::default();
        let queued_epoch = first_epoch.next();
        let mut queue = SaveQueue::new(Config::default());
        let first = queue
            .submit(SaveRequest {
                draft: Config {
                    threads: 4,
                    ..valid_config(directory.path())?
                },
                epoch: first_epoch,
                path: path.clone(),
            })
            .context("The first save was queued")?;
        let unconfigured = SaveRequest {
            draft: Config::default(),
            epoch: queued_epoch,
            path,
        };
        assert!(queue.submit(unconfigured).is_none());
        let queued = queue.finish(write_result(first).await?);
        let SaveProgress::Finished {
            saved,
            error,
            epoch,
        } = queue.finish(write_result(queued).await?)
        else {
            bail!("Failed save did not finish");
        };
        let saved = saved.context("Previous durable save was lost")?;
        assert_eq!(saved.validated.threads, 4);
        assert!(error.is_some(), "The queued failure was not reported");
        assert_eq!(
            epoch, queued_epoch,
            "A drained queue reports the epoch of its latest request"
        );
        assert!(queue.is_idle());
        assert!(queue.saved.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn a_revert_queued_behind_a_write_reaches_the_file() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let original = valid_config(directory.path())?;
        Config::save_changes(&original, &original, &original, &path)?;
        let request = |theme| SaveRequest {
            draft: Config {
                theme,
                ..original.clone()
            },
            epoch: ConfigEpoch::default(),
            path: path.clone(),
        };
        let mut queue = SaveQueue::new(original.clone());
        let first = queue
            .submit(request(Theme::Iris))
            .context("The first save was queued")?;
        assert!(queue.submit(request(original.theme)).is_none());
        let revert = queue.finish(write_result(first).await?);
        let SaveProgress::Finished { error, .. } = queue.finish(write_result(revert).await?) else {
            bail!("The revert did not finish");
        };
        assert!(error.is_none());
        assert_eq!(Config::read(&path)?.theme, original.theme);
        Ok(())
    }

    #[test]
    fn save_completion_preserves_new_edits() {
        let submitted = Config {
            threads: 4,
            ..Config::default()
        };
        let mut draft = submitted.clone();
        let mut validated = submitted.clone();
        validated.model = PathBuf::from("/resolved/model.gguf");
        let saved = SavedConfig {
            submitted,
            validated: ValidConfig::assume(validated),
        };
        draft.threads = 8;
        assert!(!saved.update_draft(&mut draft));
        assert_eq!(draft.threads, 8);
        assert!(draft.model.as_os_str().is_empty());
        draft = saved.submitted.clone();
        assert!(saved.update_draft(&mut draft));
        assert!(draft == *saved.validated);
    }

    #[test]
    fn save_worker_returns_before_storage_and_joins_owned_work() -> anyhow::Result<()> {
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let work = SaveWork::spawn(move || {
            entered.send(())?;
            released.recv()?;
            Ok(durable(Config::default()))
        })?;
        started.recv_timeout(Duration::from_secs(2))?;
        assert!(work.result.try_recv().is_err());
        let (joining, joined) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        let cleanup = std::thread::spawn(move || {
            joining.send(()).unwrap();
            drop(work);
            finished.send(()).unwrap();
        });
        joined.recv_timeout(Duration::from_secs(2))?;
        assert!(completion.recv_timeout(Duration::from_millis(10)).is_err());
        release.send(())?;
        completion.recv_timeout(Duration::from_secs(2))?;
        cleanup.join().unwrap();
        Ok(())
    }

    #[test]
    fn flush_finishes_the_running_write_before_a_revert_queued_behind_it() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let original = valid_config(directory.path())?;
        let first = Config {
            theme: Theme::Iris,
            ..original.clone()
        };
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (base, first_path) = (original.clone(), path.clone());
        let running = SaveWork::spawn(move || {
            entered.send(())?;
            released.recv()?;
            Config::save_changes(&base, &first, &first, &first_path)?;
            Ok(durable(first))
        })?;
        started.recv_timeout(Duration::from_secs(2))?;
        let mut queue = SaveQueue {
            writing: Some(running),
            pending: Some(SaveRequest {
                draft: original.clone(),
                epoch: ConfigEpoch::default(),
                path: path.clone(),
            }),
            ..SaveQueue::new(original.clone())
        };
        let cleanup = std::thread::spawn(move || queue.flush());
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !path.exists(),
            "The queued save started before the running write finished"
        );
        release.send(())?;
        cleanup.join().unwrap();
        assert_eq!(Config::read(&path)?.theme, original.theme);
        Ok(())
    }
}
