use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use speakeasy_show::{
    Annotations, Ending, Manifest, Moment, MoviePrefix, NarrationLoss, Recording, Source,
    locate::Focus,
};

use super::{
    narration::Narration,
    navigation::{Relation, Stamp},
};
use crate::show::{
    Interrupted,
    preview::Content,
    store::{
        age, checkpoint, movie,
        recall::{self, Entry},
        retention::{Retention, interval},
        source,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Keyframes,
    Source,
    Video,
}

#[derive(Debug)]
pub(super) enum Kind {
    Saved,
    Interrupted(Interrupted),
}

#[derive(Debug)]
pub(super) struct Row {
    pub kind: Kind,
    pub directory: PathBuf,
    pub readable: bool,
    pub duration: Option<Moment>,
    pub frames: usize,
    pub marks: usize,
    pub pinned: Option<bool>,
    pub early: bool,
    pub narration_lost: Option<NarrationLoss>,
}

impl Row {
    fn from_entry(entry: Entry) -> Self {
        let show = entry.show.as_ref().ok();
        let recording = show.map(|show| show.manifest.recording());
        Self {
            kind: Kind::Saved,
            readable: show.is_some(),
            duration: recording.map(|recording| recording.duration),
            frames: show.map_or(0, |show| show.manifest.keyframes().len()),
            marks: show.map_or(0, |show| show.manifest.marks().len()),
            pinned: Some(entry.pinned),
            early: recording.is_some_and(|recording| recording.ending != Ending::Finished),
            narration_lost: recording.and_then(|recording| recording.narration_lost),
            directory: entry.directory,
        }
    }

    pub(super) fn name(&self) -> &str {
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
        let Some(recorded) = recorded else {
            return self.name().to_owned();
        };
        match now.duration_since(recorded) {
            Err(_) => "Recorded later than this clock".into(),
            Ok(age) if age.as_secs() < 60 => "Just now".into(),
            Ok(age) => format!("{} ago", interval(age)),
        }
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

pub(super) fn list(directory: &Path) -> io::Result<Vec<Row>> {
    let interrupted = checkpoint::list(directory, SystemTime::now())
        .into_iter()
        .map(|interrupted| Row {
            directory: interrupted.directory().to_owned(),
            kind: Kind::Interrupted(interrupted),
            readable: true,
            duration: None,
            frames: 0,
            marks: 0,
            pinned: None,
            early: true,
            narration_lost: None,
        });
    let saved = recall::list(directory)?.map(|entry| entry.map(Row::from_entry));
    interrupted.map(Ok).chain(saved).collect()
}

#[derive(Debug)]
pub(super) struct Frame {
    pub number: usize,
    pub presentation: Duration,
    pub marker: String,
    pub cropped: bool,
    pub content: Content,
}

impl Frame {
    pub(super) fn at(&self) -> Moment {
        Moment::from(self.presentation)
    }

    pub(super) fn caption(&self, count: usize) -> String {
        let crop = if self.cropped {
            " · Cropped to where you worked"
        } else {
            ""
        };
        format!(
            "#{} of {count} · {}{}{crop}",
            self.number,
            Stamp(self.at()),
            self.marker
        )
    }

    pub(super) const fn offers_whole_screen(&self, source_available: bool) -> bool {
        self.cropped && source_available
    }
}

pub(super) struct Detail {
    pub frames: Vec<Frame>,
    pub marks: Vec<(usize, Moment)>,
    pub warnings: Vec<String>,
    pub summary: Result<String, String>,
    pub narration: Narration,
    pub source_available: bool,
    pub coverage: String,
    pub movie: Option<movie::Metadata>,
}

impl Detail {
    pub(super) fn locate(&self, at: Moment) -> Option<(usize, Relation)> {
        let index = self
            .frames
            .partition_point(|frame| {
                movie::decoder_time(frame.presentation) <= at.since(Moment::START)
            })
            .saturating_sub(1);
        Some((
            index,
            Relation::between(at, self.frames.get(index)?.presentation),
        ))
    }

    pub(super) fn open(directory: &Path, mode: Mode) -> io::Result<Self> {
        let entry = Entry::open(directory, None)?;
        let summary = summary(directory);
        let mut warnings = Vec::new();
        let mut frames = Vec::new();
        let mut marks = Vec::new();
        let mut narration = Narration::Unavailable;
        let mut inspected = None;
        let mut scene = None;
        let mut duration = None;
        match entry.show {
            Err(error) => warnings.push(format!(
                "Cannot read this Show: {error:#}. Its summary may still be readable."
            )),
            Ok(show) => {
                inspected = show.movie;
                let manifest = show.manifest;
                let recording = manifest.recording();
                duration = Some(recording.duration);
                if mode == Mode::Video {
                    match Annotations::new(recording, manifest.marks()) {
                        Ok(annotations) => scene = Some(Arc::new(annotations)),
                        Err(error) => {
                            warnings.push(format!("Video annotations are unavailable: {error}"));
                        },
                    }
                }
                narration = Narration::from_recording(recording);
                warnings.extend(ending(recording));
                warnings.extend(recording.narration_lost.map(lost));
                for (index, mark) in manifest.marks().iter().enumerate() {
                    if let Ok(spans) = manifest.locate(Focus::Mark(mark.number))
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
                if mode == Mode::Keyframes {
                    frames = keyframes(&manifest);
                }
            },
        }
        let mut source_available = false;
        let mut coverage = "This Show has no retained source frames.".to_owned();
        match source::present(directory) {
            Ok(false) => {},
            Err(error) => warnings.push(format!("Source frames are unavailable: {error}")),
            Ok(true) => match source::inspect(directory) {
                Err(error) => warnings.push(format!("Source frames are unavailable: {error:#}")),
                Ok(source) if Some(source.duration()) != duration => warnings
                    .push("Source frames do not match this Show's recording timeline.".into()),
                Ok(source) => {
                    source_available = true;
                    coverage = format!(
                        "{} retained samples from {} arrivals · longest gap {} ms · source images \
                         have no annotations",
                        source.moments().len(),
                        source.arrivals(),
                        source::longest_gap(&source)
                    );
                    if mode == Mode::Source {
                        frames = retained(source);
                    }
                },
            },
        }
        if mode == Mode::Video
            && let Some(movie::Inspected { metadata, prefix }) = &inspected
        {
            let annotations = Moment::from_millis(
                metadata
                    .coverage
                    .as_ref()
                    .map_or(0, |coverage| coverage.annotations_through_ms),
            );
            frames = sampled(
                Duration::from_millis(metadata.origin_ms),
                prefix,
                annotations,
                scene,
            );
            coverage = format!(
                "{} video samples · annotation record through {}",
                frames.len(),
                Stamp(annotations)
            );
        }
        Ok(Self {
            frames,
            marks,
            warnings,
            summary,
            narration,
            source_available,
            coverage,
            movie: inspected.map(|inspected| inspected.metadata),
        })
    }
}

pub(super) fn summary(directory: &Path) -> Result<String, String> {
    recall::summary(directory).map_err(|error| format!("Cannot read summary.md: {error}"))
}

fn ending(recording: &Recording) -> Option<String> {
    let duration = recording.duration;
    match recording.ending {
        Ending::Finished => None,
        Ending::ScreenStopped => Some(format!(
            "Screen recording stopped at {duration}, before you finished."
        )),
        Ending::WindowClosed => Some(format!("The recorded window closed at {duration}.")),
        Ending::WindowOffScreen => Some(format!(
            "The recorded window left the screen at {duration}."
        )),
    }
}

fn lost(loss: NarrationLoss) -> String {
    match loss {
        NarrationLoss::Incomplete => {
            "Narration is incomplete: speech after the audio stopped is missing."
        },
        NarrationLoss::AudioStopped => "The microphone stopped, so this Show has no narration.",
        NarrationLoss::AudioSilent => {
            "The microphone sent only silence, so this Show has no narration."
        },
        NarrationLoss::NotTranscribed => {
            "Your speech couldn't be transcribed, so this Show has no narration."
        },
    }
    .to_owned()
}

fn keyframes(manifest: &Manifest) -> Vec<Frame> {
    manifest
        .keyframes()
        .iter()
        .map(|frame| Frame {
            number: frame.index,
            presentation: frame.at.since(Moment::START),
            marker: frame.marker(manifest.marks()),
            cropped: frame.crop.is_some(),
            content: Content::Png(frame.file()),
        })
        .collect()
}

fn retained(source: Source) -> Vec<Frame> {
    let source = Arc::new(source);
    source
        .moments()
        .iter()
        .enumerate()
        .map(|(index, &at)| Frame {
            number: index.saturating_add(1),
            presentation: at.since(Moment::START),
            marker: String::new(),
            cropped: false,
            content: Content::Source {
                index: Arc::clone(&source),
            },
        })
        .collect()
}

fn sampled(
    origin: Duration,
    prefix: &MoviePrefix,
    annotations: Moment,
    scene: Option<Arc<Annotations>>,
) -> Vec<Frame> {
    let samples = prefix.samples();
    let ends = samples
        .iter()
        .skip(1)
        .map(|next| next.at)
        .chain([prefix.duration()]);
    samples
        .iter()
        .zip(ends)
        .enumerate()
        .map(|(index, (sample, end))| Frame {
            number: index.saturating_add(1),
            presentation: origin.saturating_add(sample.at),
            marker: String::new(),
            cropped: false,
            content: Content::Video {
                span: sample.at..end,
                annotations,
                scene: scene.clone(),
            },
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Key {
    revision: u64,
    pub directory: PathBuf,
    pub frame: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Offered {
    revision: u64,
    pub index: usize,
}

#[derive(Default)]
enum Selection {
    #[default]
    Unvisited,
    Selected(Key),
    Gone,
}

#[derive(Default)]
pub(super) struct Library {
    pub rows: Vec<Row>,
    selection: Selection,
    revision: u64,
    listing_revision: u64,
}

impl Library {
    pub(super) fn update_retention(&mut self, directory: &Path, pinned: Option<bool>) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.directory == directory) {
            row.pinned = pinned;
        }
    }

    pub(super) fn replace(&mut self, rows: Vec<Row>) -> Option<Key> {
        self.listing_revision = self.listing_revision.wrapping_add(1);
        self.rows = rows;
        let (directory, frame) = match &self.selection {
            Selection::Selected(key)
                if self.rows.iter().any(|row| row.directory == key.directory) =>
            {
                (key.directory.clone(), key.frame)
            },
            Selection::Selected(_) => {
                self.selection = Selection::Gone;
                return None;
            },
            Selection::Unvisited => (self.rows.first()?.directory.clone(), 0),
            Selection::Gone => return None,
        };
        Some(self.select(directory, frame))
    }

    pub(super) const fn offer(&self, index: usize) -> Offered {
        Offered {
            revision: self.listing_revision,
            index,
        }
    }

    pub(super) fn offered(&self, offer: Offered) -> Option<&Row> {
        self.rows
            .get(offer.index)
            .filter(|_| offer.revision == self.listing_revision)
    }

    pub(super) fn select(&mut self, directory: PathBuf, frame: usize) -> Key {
        self.revision = self.revision.wrapping_add(1);
        let key = Key {
            revision: self.revision,
            directory,
            frame,
        };
        self.selection = Selection::Selected(key.clone());
        key
    }

    pub(super) const fn selected(&self) -> Option<&Key> {
        match &self.selection {
            Selection::Selected(key) => Some(key),
            Selection::Unvisited | Selection::Gone => None,
        }
    }

    pub(super) const fn is_gone(&self) -> bool {
        matches!(self.selection, Selection::Gone)
    }

    pub(super) fn accepts(&self, key: &Key) -> bool {
        self.selected() == Some(key)
    }

    pub(super) fn position(&self) -> Option<usize> {
        let key = self.selected()?;
        self.rows
            .iter()
            .position(|row| row.directory == key.directory)
    }

    pub(super) fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.position()?)
    }

    pub(super) fn is_selected(&self, directory: &Path) -> bool {
        self.selected()
            .is_some_and(|key| key.directory == directory)
    }

    pub(super) fn forget(&mut self) {
        if let Selection::Selected(key) = std::mem::replace(&mut self.selection, Selection::Gone) {
            self.remove(&key.directory);
        }
    }

    pub(super) fn remove(&mut self, directory: &Path) -> bool {
        self.listing_revision = self.listing_revision.wrapping_add(1);
        self.rows.retain(|row| row.directory != directory);
        let selected = self.is_selected(directory);
        if selected {
            self.selection = Selection::Gone;
        }
        selected
    }
}
