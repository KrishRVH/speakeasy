//! A native writer's independent lease survives failed frame/audio lanes and keeper unwinding.

use std::{path::Path, time::Instant};

use anyhow::Context as _;
use speakeasy_platform::{MovieControl, MovieEvent, MovieTarget};

use super::super::super::movie::{FILE, Metadata, geometry};

pub(super) struct Writing {
    control: MovieControl,
    started: Instant,
    recording: Option<(Metadata, geometry::Writing)>,
}

impl Writing {
    pub(super) fn new(directory: &Path, started: Instant) -> anyhow::Result<(Self, MovieTarget)> {
        let deadline = started
            .checked_add(speakeasy_core::gesture::RECORDING_LIMIT)
            .context("Movie deadline is out of range")?;
        let (control, target) = MovieControl::new(directory.join(FILE), deadline);
        Ok((
            Self {
                control,
                started,
                recording: None,
            },
            target,
        ))
    }

    pub(super) fn stop(&self) {
        self.control.request_stop();
    }

    pub(super) fn event(&mut self, directory: &Path, event: MovieEvent) {
        match event {
            MovieEvent::Started { at, size, accepted } => {
                let fresh = self.recording.is_none();
                if self.recording.is_none() {
                    self.recording = Metadata::begin(directory, self.started, at, size).ok();
                } else {
                    self.stop();
                }
                let ready = fresh && self.recording.is_some();
                if !ready || accepted.try_send(ready).is_err() {
                    self.stop();
                }
            },
            MovieEvent::Geometry {
                sample,
                content,
                part,
                accepted,
            } => {
                let saved = self.recording.as_mut().is_some_and(|(_, geometry)| {
                    geometry
                        .record(
                            sample,
                            content,
                            [part.left, part.top, part.right, part.bottom],
                        )
                        .is_ok()
                });
                // A rejected receipt reports persistence failure on the capture thread. Cancelling
                // first would race that refusal and turn it into an ordinary, silent stop.
                if accepted.try_send(saved).is_err() {
                    self.stop();
                }
            },
            MovieEvent::Finished {
                last,
                through,
                frames,
                finalized,
            } => {
                if let Some((metadata, _)) = &mut self.recording
                    && metadata
                        .finish(directory, self.started, last, through, frames, finalized)
                        .is_err()
                {
                    self.stop();
                }
            },
        }
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        // This owner lives only on the checkpoint thread. Even unwinding must acknowledge the
        // native encoder's file closure before the outer Checkpoint releases its process lock.
        self.control.close_blocking();
    }
}

pub(super) async fn next(writing: Option<&Writing>) -> Option<MovieEvent> {
    match writing {
        Some(writing) => writing.control.next().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_is_on_disk_before_acceptance_and_refusal_remains_an_error() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let started = Instant::now();
        let (mut writing, target) = Writing::new(directory.path(), started)?;
        let (accepted, answer) = async_channel::bounded(1);
        writing.event(
            directory.path(),
            MovieEvent::Started {
                at: started,
                size: [4, 2],
                accepted,
            },
        );
        assert!(answer.try_recv()?);
        let journal = directory.path().join(geometry::FILE);
        let before = std::fs::read(&journal)?;
        for (sample, expected) in [(0, true), (0, false)] {
            let (accepted, answer) = async_channel::bounded(1);
            writing.event(
                directory.path(),
                MovieEvent::Geometry {
                    sample,
                    content: [0, 0, 4, 2],
                    part: speakeasy_platform::Part::WHOLE,
                    accepted,
                },
            );
            let saved = std::fs::read(&journal)?;
            assert_eq!(saved.len(), before.len() + 40);
            assert!(saved.starts_with(&before));
            assert_eq!(answer.try_recv()?, expected);
            assert!(
                !target.cancelled(),
                "Cancellation would mask the failed receipt as an ordinary stop"
            );
        }
        drop(target);
        Ok(())
    }

    #[test]
    fn acceptance_requires_saved_origin_and_failed_storage_cancels_the_target() -> anyhow::Result<()>
    {
        for refused in [false, true] {
            let directory = tempfile::tempdir()?;
            let started = Instant::now();
            let (mut writing, target) = Writing::new(directory.path(), started)?;
            let (accepted, answer) = async_channel::bounded(1);
            let header = directory
                .path()
                .join(super::super::super::super::movie::HEADER);
            if refused {
                std::fs::create_dir(&header)?;
            }
            writing.event(
                directory.path(),
                MovieEvent::Started {
                    at: started,
                    size: [4, 2],
                    accepted,
                },
            );
            assert_eq!(answer.try_recv().unwrap_or(false), !refused);
            assert_eq!(target.cancelled(), refused);
            if !refused {
                let metadata: Metadata = serde_json::from_slice(&std::fs::read(header)?)?;
                assert_eq!(metadata.origin_ms, 0);
                assert_eq!(metadata.size, [4, 2]);
            }
            drop(target);
        }
        Ok(())
    }
}
