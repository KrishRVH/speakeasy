//! Content-free navigation and projections of saved Shows. Late reads never change a newer choice.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use speakeasy_show::{Ending, Moment, NarrationLoss, Unreadable};

use crate::show::store::{
    age, checkpoint, pinned,
    recall::{self, Entry, Health, ReadError},
    retention::{Retention, interval},
    source,
};

const INCOMPLETE_NARRATION: &str =
    "Narration is incomplete: only audio that reached the checkpoint was recovered.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Mode {
    #[default]
    Keyframes,
    Source,
}

#[derive(Debug)]
pub(super) enum Kind {
    Saved,
    Interrupted(crate::show::Interrupted),
}

#[derive(Debug)]
pub(super) struct Row {
    pub kind: Kind,
    pub directory: PathBuf,
    pub health: Health,
    pub duration: Option<Moment>,
    pub frames: usize,
    pub marks: usize,
    pub pinned: Option<bool>,
    pub early: bool,
    pub narration_lost: Option<NarrationLoss>,
}

impl Row {
    fn from_entry(entry: &Entry) -> Self {
        let show = entry.show.as_ref().ok();
        Self {
            kind: Kind::Saved,
            directory: entry.directory.clone(),
            health: entry.health(),
            duration: show.map(|show| show.manifest.recording().duration),
            frames: show.map_or(0, |show| show.manifest.keyframes().len()),
            marks: show.map_or(0, |show| show.manifest.marks().len()),
            pinned: pinned(&entry.directory).ok(),
            early: show.is_some_and(|show| show.manifest.recording().ending != Ending::Finished),
            narration_lost: show.and_then(|show| show.manifest.recording().narration_lost),
        }
    }

    fn name(&self) -> &str {
        self.directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Show")
    }

    pub(super) fn recorded(&self, now: SystemTime) -> String {
        let recorded = match &self.kind {
            Kind::Interrupted(show) => Some(show.recorded()),
            Kind::Saved => age::recorded_at(self.name()),
        };
        recorded.map_or_else(
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
        let retention = match (&self.kind, self.pinned) {
            (Kind::Interrupted(_), _) => Retention::of(self.name(), false),
            (Kind::Saved, Some(kept)) => Retention::of(self.name(), kept),
            (Kind::Saved, None) => Retention::Unknown,
        };
        retention.label(now)
    }
}

pub(super) fn list(directory: &Path) -> io::Result<(Vec<Row>, Option<String>)> {
    let found = checkpoint::list(directory, SystemTime::now());
    let notice = (!found.problems.is_empty()).then(|| {
        format!(
            "Cannot read {} interrupted Show entries. Check folder access and refresh. Damaged \
entries expire after seven days. {}",
            found.problems.len(),
            found
                .problems
                .first()
                .map_or("", |problem| problem.message.as_str())
        )
    });
    let mut rows: Vec<_> = found
        .shows
        .into_iter()
        .map(|interrupted| Row {
            directory: interrupted.directory().to_owned(),
            kind: Kind::Interrupted(interrupted),
            health: Health::Ok,
            duration: None,
            frames: 0,
            marks: 0,
            pinned: None,
            early: true,
            narration_lost: None,
        })
        .collect();
    rows.extend(
        recall::list(directory)?
            .map(|entry| entry.map(|entry| Row::from_entry(&entry)))
            .collect::<io::Result<Vec<_>>>()?,
    );
    Ok((rows, notice))
}

#[derive(Debug)]
pub(super) struct Frame {
    pub number: usize,
    pub at: Moment,
    pub marker: String,
    pub file: super::frame::Content,
}

/// The only private text retained is the user's current summary. Never derive Debug for it.
pub(super) struct Detail {
    pub frames: Vec<Frame>,
    pub marks: Vec<(usize, Moment)>,
    pub warnings: Vec<String>,
    pub summary: Result<String, String>,
    pub source_available: bool,
    pub coverage: String,
}

impl Detail {
    pub(super) fn frame_at(&self, at: Moment) -> usize {
        self.frames
            .partition_point(|frame| frame.at <= at)
            .saturating_sub(1)
    }

    pub(super) fn open(directory: &Path, mode: Mode) -> io::Result<Self> {
        let entry = Entry::open(directory)?;
        let summary =
            recall::summary(directory).map_err(|error| format!("Cannot read summary.md: {error}"));
        let mut warnings = Vec::new();
        let mut frames = Vec::new();
        let mut marks = Vec::new();
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
                    Some(NarrationLoss::Incomplete) => warnings.push(INCOMPLETE_NARRATION.into()),
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
                    if let Ok(spans) = manifest.locate(speakeasy_show::locate::Focus::Mark(mark.number))
                        && let Some(span) = spans.first()
                    {
                        marks.push((mark.number, span.start()));
                    }
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
                        file: super::frame::Content::Png(frame.file()),
                    })
                    .collect();
            },
        }
        let mut source_available = false;
        let mut coverage = "This Show has no retained source frames.".to_owned();
        match source::size(directory) {
            Ok(None) => {},
            Err(error) => warnings.push(format!("Source frames are unavailable: {error}")),
            Ok(Some(_)) => match source::inspect(directory) {
                Err(error) => warnings.push(format!("Source frames are unavailable: {error:#}")),
                Ok(source) => {
                    source_available = true;
                    coverage = format!(
                        "{} retained samples from {} arrivals · longest gap {} ms · source images have no annotations",
                        source.moments().len(),
                        source.arrivals(),
                        source::longest_gap(&source)
                    );
                    if mode == Mode::Source {
                        let source = Arc::new(source);
                        frames = source
                            .moments()
                            .iter()
                            .enumerate()
                            .map(|(index, &at)| Frame {
                                number: index.saturating_add(1),
                                at,
                                marker: String::new(),
                                file: super::frame::Content::Source {
                                    index: Arc::clone(&source),
                                    at,
                                },
                            })
                            .collect();
                    }
                },
            },
        }
        if mode == Mode::Source && !source_available {
            frames.clear();
        }
        Ok(Self {
            frames,
            marks,
            warnings,
            summary,
            source_available,
            coverage,
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
