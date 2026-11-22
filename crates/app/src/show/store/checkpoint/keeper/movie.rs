use std::{path::Path, time::Instant};

use anyhow::Context as _;
use speakeasy_core::gesture::RECORDING_LIMIT;
use speakeasy_platform::{MovieControl, MovieEvent, MovieTarget};

use crate::show::store::movie::{FILE, Metadata, geometry};

pub(super) struct Writing {
    control: MovieControl,
    started: Instant,
    recording: Option<(Metadata, geometry::Writing)>,
}

impl Writing {
    pub(super) fn new(directory: &Path, started: Instant) -> anyhow::Result<(Self, MovieTarget)> {
        let deadline = started
            .checked_add(RECORDING_LIMIT)
            .context("Movie deadline is out of range")?;
        let (control, target) = MovieControl::new(directory.join(FILE), deadline, RECORDING_LIMIT);
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
                if fresh {
                    self.recording = Metadata::begin(directory, self.started, at, size).ok();
                }
                if !fresh || self.recording.is_none() || accepted.send(true).is_err() {
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

                if accepted.send(saved).is_err() {
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
    use tokio::sync::oneshot;

    use super::*;

    #[test]
    fn a_started_movie_saves_its_origin_before_accepting() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let started = Instant::now();
        let (mut writing, target) = Writing::new(directory.path(), started)?;
        let (accepted, mut answer) = oneshot::channel();
        writing.event(
            directory.path(),
            MovieEvent::Started {
                at: started,
                size: [4, 2],
                accepted,
            },
        );
        assert!(answer.try_recv()?);
        let header = directory.path().join(crate::show::store::movie::HEADER);
        let metadata: Metadata = serde_json::from_slice(&std::fs::read(header)?)?;
        assert_eq!(metadata.origin_ms, 0);
        assert_eq!(metadata.size, [4, 2]);
        drop(target);
        Ok(())
    }
}
