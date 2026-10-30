//! Content-free navigation and projections of saved Shows. Late reads never change a newer choice.

use std::{
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use speakeasy_show::{Ending, Moment, NarrationLoss, Unreadable};

use crate::show::store::{
    age, pinned,
    recall::{self, Entry, Health, ReadError},
    retention::{Retention, interval},
};

#[derive(Debug)]
pub(super) struct Row {
    pub directory: PathBuf,
    pub health: Health,
    pub duration: Option<Moment>,
    pub frames: usize,
    pub marks: usize,
    pub pinned: Option<bool>,
    pub early: bool,
    pub silent: bool,
}

impl Row {
    fn from_entry(entry: &Entry) -> Self {
        let show = entry.show.as_ref().ok();
        Self {
            directory: entry.directory.clone(),
            health: entry.health(),
            duration: show.map(|show| show.manifest.recording().duration),
            frames: show.map_or(0, |show| show.manifest.keyframes().len()),
            marks: show.map_or(0, |show| show.manifest.marks().len()),
            pinned: pinned(&entry.directory).ok(),
            early: show.is_some_and(|show| show.manifest.recording().ending != Ending::Finished),
            silent: show.is_some_and(|show| show.manifest.recording().narration_lost.is_some()),
        }
    }

    fn name(&self) -> &str {
        self.directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Show")
    }

    pub(super) fn recorded(&self, now: SystemTime) -> String {
        age::recorded_at(self.name()).map_or_else(
            || self.name().to_owned(),
            |recorded| {
                now.duration_since(recorded).map_or_else(
                    |_| "Recorded later than this clock".into(),
                    |age| {
                        if age.as_secs() < 60 {
                            "Just now".into()
                        } else {
                            format!("{} ago", interval(age))
                        }
                    },
                )
            },
        )
    }

    pub(super) fn retention(&self, now: SystemTime) -> String {
        let retention = match self.pinned {
            Some(kept) => Retention::of(self.name(), kept),
            None => Retention::Unknown,
        };
        retention.label(now)
    }
}

pub(super) fn list(directory: &Path) -> io::Result<Vec<Row>> {
    recall::list(directory)?
        .map(|entry| entry.map(|entry| Row::from_entry(&entry)))
        .collect()
}

#[derive(Debug)]
pub(super) struct Frame {
    pub number: usize,
    pub at: Moment,
    pub marker: String,
    pub file: String,
}

/// The only private text retained is the user's current summary. Never derive Debug for it.
pub(super) struct Detail {
    pub frames: Vec<Frame>,
    pub warnings: Vec<String>,
    pub summary: Result<String, String>,
}

impl Detail {
    pub(super) fn open(directory: &Path) -> io::Result<Self> {
        let entry = Entry::open(directory)?;
        let summary =
            recall::summary(directory).map_err(|error| format!("Cannot read summary.md: {error}"));
        let mut warnings = Vec::new();
        let mut frames = Vec::new();
        match entry.show {
            Err(ReadError::Manifest(Unreadable::Unsupported(version))) => warnings.push(format!(
                "This Show uses format {version}. Update Speakeasy to view its frames. Its summary may still be readable."
            )),
            Err(error) => warnings.push(format!(
                "Cannot read this Show's frame list: {error}. Its summary may still be readable."
            )),
            Ok(show) => {
                let manifest = show.manifest;
                let recording = manifest.recording();
                let duration = recording.duration;
                match recording.ending {
                    Ending::Finished => {},
                    Ending::ScreenStopped => warnings.push(format!(
                        "Screen recording stopped at {duration}, before you finished."
                    )),
                    Ending::WindowClosed => {
                        warnings.push(format!("The recorded window closed at {duration}."));
                    },
                    Ending::WindowOffScreen => warnings.push(format!(
                        "The recorded window left the screen at {duration}."
                    )),
                }
                match recording.narration_lost {
                    None => {},
                    Some(NarrationLoss::AudioStopped) => warnings
                        .push("The microphone stopped, so this Show has no narration.".into()),
                    Some(NarrationLoss::NotTranscribed) => warnings.push(
                        "Your speech couldn't be transcribed, so this Show has no narration."
                            .into(),
                    ),
                }
                warnings.extend(
                    show.problems
                        .into_iter()
                        .map(|problem| format!("{}: {}", problem.name, problem.reason)),
                );
                for (index, mark) in manifest.marks().iter().enumerate() {
                    if !manifest
                        .keyframes()
                        .iter()
                        .any(|frame| frame.marks.contains(&index))
                    {
                        warnings.push(format!("Mark {} has no saved frame.", mark.number));
                    }
                }
                frames = manifest
                    .keyframes()
                    .iter()
                    .map(|frame| Frame {
                        number: frame.index,
                        at: frame.at,
                        marker: frame.marker(manifest.marks()),
                        file: frame.file(),
                    })
                    .collect();
            },
        }
        Ok(Self {
            frames,
            warnings,
            summary,
        })
    }
}

/// A read's exact selection, including revisiting the same Show or frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Key {
    revision: u64,
    pub directory: PathBuf,
    pub frame: usize,
}

#[derive(Default)]
pub(super) struct Library {
    pub rows: Vec<Row>,
    pub selected: Option<Key>,
    pub gone: bool,
    visited: bool,
    revision: u64,
}

impl Library {
    pub(super) fn update_retention(&mut self, directory: &Path, pinned: Option<bool>) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.directory == directory) {
            row.pinned = pinned;
        }
    }

    pub(super) fn replace(&mut self, rows: Vec<Row>) -> Option<Key> {
        self.rows = rows;
        let path = match &self.selected {
            Some(key) if self.rows.iter().any(|row| row.directory == key.directory) => {
                Some(key.directory.clone())
            },
            Some(_) => {
                self.forget();
                None
            },
            None if !self.visited => self.rows.first().map(|row| row.directory.clone()),
            None => None,
        };
        let frame = self.selected.as_ref().map_or(0, |key| key.frame);
        path.map(|path| self.select(path, frame))
    }

    pub(super) fn select(&mut self, directory: PathBuf, frame: usize) -> Key {
        // A revision identifies work, not a count; wrapping cannot meet a live read 2^64 choices old.
        self.revision = self.revision.wrapping_add(1);
        let key = Key {
            revision: self.revision,
            directory,
            frame,
        };
        self.selected = Some(key.clone());
        self.visited = true;
        self.gone = false;
        key
    }

    pub(super) fn accepts(&self, key: &Key) -> bool {
        self.selected.as_ref() == Some(key)
    }

    pub(super) fn is_selected(&self, directory: &Path) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|key| key.directory == directory)
    }

    pub(super) fn forget(&mut self) {
        if let Some(key) = self.selected.take() {
            self.rows.retain(|row| row.directory != key.directory);
        }
        self.gone = true;
        self.visited = true;
    }
}
