//! Silent movie storage beside a Show.
//!
//! Coverage comes from the container's surviving fragments, not from the writer's `Finished`
//! receipt, which only marks coverage partial when it is missing or unfinalized.

mod frame;
pub(in crate::show::store) mod geometry;
use std::{
    fs::{self, File},
    io::{Read as _, Seek as _, Write as _},
    path::Path,
    time::Instant,
};

use anyhow::Context as _;
pub(crate) use frame::{AnnotationState, Crop, decoder_time};
use serde::{Deserialize, Serialize};
use speakeasy_core::gesture::RECORDING_LIMIT;
use speakeasy_show::{Moment, MoviePrefix};

use super::found;

pub(crate) const FILE: &str = "recording.mp4";
pub(crate) const HEADER: &str = "recording.json";
const VERSION: u8 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Metadata {
    version: u8,
    identity: [u8; 16],
    pub origin_ms: u64,
    pub size: [u32; 2],
    pub finished: Option<Finished>,
    /// Set by `retain` at publication; `None` in a live or crashed checkpoint.
    pub coverage: Option<Coverage>,
    pub geometry: Option<geometry::Geometry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Finished {
    pub last_ms: u64,
    pub through_ms: u64,
    pub submitted: u32,
    pub finalized: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Coverage {
    pub end_ms: u64,
    pub frames: u32,
    pub bytes: u64,
    pub annotations_through_ms: u64,
    pub partial: bool,
}

impl Metadata {
    pub(in crate::show::store) fn begin(
        directory: &Path,
        started: Instant,
        at: Instant,
        size: [u32; 2],
    ) -> anyhow::Result<(Self, geometry::Writing)> {
        let origin = at
            .checked_duration_since(started)
            .context("Movie began before its session")?;
        anyhow::ensure!(
            origin < RECORDING_LIMIT,
            "Movie began after the recording limit"
        );
        anyhow::ensure!(
            size.into_iter()
                .all(|edge| (2..=speakeasy_show::MAX_FRAME_EDGE).contains(&edge)
                    && edge.is_multiple_of(2)),
            "Movie canvas is invalid"
        );
        let mut identity = [0; 16];
        getrandom::fill(&mut identity)
            .map_err(|_unavailable| anyhow::anyhow!("Cannot create a private movie identifier"))?;
        let geometry = geometry::Writing::create(directory, identity, size)?;
        let metadata = Self {
            version: VERSION,
            identity,
            origin_ms: u64::try_from(origin.as_millis())?,
            size,
            finished: None,
            coverage: None,
            geometry: None,
        };
        File::create_new(directory.join(HEADER))?.write_all(&serde_json::to_vec(&metadata)?)?;
        Ok((metadata, geometry))
    }

    pub(in crate::show::store) fn finish(
        &mut self,
        directory: &Path,
        started: Instant,
        last: Instant,
        through: Instant,
        submitted: u32,
        finalized: bool,
    ) -> anyhow::Result<()> {
        let last_ms = u64::try_from(last.saturating_duration_since(started).as_millis())?;
        let through_ms = u64::try_from(
            through
                .saturating_duration_since(started)
                .min(RECORDING_LIMIT)
                .as_millis(),
        )?;
        self.finished = Some(Finished {
            last_ms,
            through_ms,
            submitted,
            finalized,
        });
        self.replace(directory)
    }

    fn replace(&self, directory: &Path) -> anyhow::Result<()> {
        let mut staging = tempfile::NamedTempFile::new_in(directory)?;
        staging.write_all(&serde_json::to_vec(self)?)?;
        staging
            .persist(directory.join(HEADER))
            .map_err(|error| error.error)?;
        Ok(())
    }

    pub(crate) fn note(&self, path: &Path) -> anyhow::Result<String> {
        let coverage = self
            .coverage
            .as_ref()
            .context("Movie has no retained coverage")?;
        let partial = if coverage.partial {
            "Only complete surviving fragments are retained. "
        } else {
            ""
        };
        let geometry = if self
            .geometry
            .as_ref()
            .is_some_and(|geometry| geometry.through < coverage.frames)
        {
            " Some frames have no saved crop geometry; export those frames uncropped."
        } else {
            ""
        };
        let path = code(path);
        Ok(format!(
            "\n\nSilent video: {path}. Recorded session coverage: {}–{} ms. {partial}The video has no narration audio or drawn annotations; its last image may be held through the end. Marks and narration in show.json are documented through {} ms.{geometry}\n",
            self.origin_ms, coverage.end_ms, coverage.annotations_through_ms
        ))
    }
}

#[derive(Debug)]
pub(crate) struct Inspected {
    pub metadata: Metadata,
    pub prefix: MoviePrefix,
}

pub(crate) struct Reading {
    file: File,
    pub movie: Inspected,
}

impl Reading {
    pub(crate) fn open(directory: &Path, annotations: Moment) -> anyhow::Result<Self> {
        Self::optional(directory, annotations)?
            .context("This Show has no retained video; use its source frames instead")
    }

    pub(in crate::show::store) fn optional(
        directory: &Path,
        annotations: Moment,
    ) -> anyhow::Result<Option<Self>> {
        let Some(reading) = open(directory)? else {
            return Ok(None);
        };
        let coverage = reading
            .movie
            .metadata
            .coverage
            .as_ref()
            .context("Movie has no published coverage")?;
        anyhow::ensure!(
            coverage.annotations_through_ms == annotations.as_millis(),
            "Movie annotation coverage differs from the Show"
        );
        Ok(Some(reading))
    }

    /// Copies the validated prefix with fragment headers normalized for the Mac reader.
    ///
    /// Publication, ZIP export, and video export all copy through here. Bytes past the last
    /// complete fragment, including any final index, are left out.
    pub(in crate::show::store) fn copy_to(
        &mut self,
        out: &mut impl std::io::Write,
    ) -> anyhow::Result<u64> {
        let bytes = self.movie.prefix.bytes();
        self.file.rewind()?;
        let mut position = 0_u64;
        let mut buffer = vec![0; 64 * 1024];
        while position < bytes {
            let count = usize::try_from(bytes.saturating_sub(position).min(64 * 1024))?;
            let chunk = buffer
                .get_mut(..count)
                .context("Movie copy exceeded its buffer")?;
            self.file
                .read_exact(chunk)
                .context("Movie changed during publication")?;
            self.movie.prefix.normalization().apply(position, chunk)?;
            out.write_all(chunk)?;
            position = position
                .checked_add(u64::try_from(count)?)
                .context("Movie copy position overflowed")?;
        }
        Ok(position)
    }
}

fn read_metadata(directory: &Path) -> anyhow::Result<Metadata> {
    let metadata: Metadata = serde_json::from_slice(&fs::read(directory.join(HEADER))?)
        .context("Movie timing metadata is damaged")?;
    anyhow::ensure!(
        metadata.version == VERSION,
        "Movie timing metadata is unsupported"
    );
    Ok(metadata)
}

/// No `recording.mp4` means a still-only Show; a damaged movie is an error, never absent.
pub(super) fn open(directory: &Path) -> anyhow::Result<Option<Reading>> {
    let Some(mut file) = found(File::open(directory.join(FILE)))? else {
        anyhow::ensure!(
            !fs::exists(directory.join(HEADER))?,
            "Movie timing exists without its video"
        );
        return Ok(None);
    };
    let mut metadata = read_metadata(directory)?;
    let prefix = MoviePrefix::read(file.metadata()?.len(), |range| {
        let length = range
            .end
            .checked_sub(range.start)
            .context("Movie metadata range is invalid")?;
        let mut bytes = vec![0; usize::try_from(length)?];
        file.seek(std::io::SeekFrom::Start(range.start))?;
        file.read_exact(&mut bytes)?;
        anyhow::Ok(bytes)
    })?;
    if metadata.coverage.is_none() {
        metadata.geometry = Some(geometry::recover(
            directory,
            metadata.identity,
            metadata.size,
            prefix.frames(),
        )?);
    }
    metadata
        .geometry
        .as_ref()
        .context("Video crop geometry is missing")?
        .validate(metadata.size, prefix.frames())?;
    Ok(Some(Reading {
        file,
        movie: Inspected { metadata, prefix },
    }))
}

pub(super) fn saved(directory: &Path, annotations: Moment) -> anyhow::Result<Option<Inspected>> {
    Ok(Reading::optional(directory, annotations)?.map(|reading| reading.movie))
}

pub(in crate::show::store) fn retain(
    destination: &Path,
    mut reading: Reading,
    annotations: Moment,
) -> anyhow::Result<Metadata> {
    anyhow::ensure!(
        annotations <= speakeasy_show::JOURNAL_DURATION_LIMIT,
        "Movie annotation coverage exceeds the Show journal limit"
    );
    reading.copy_to(&mut File::create_new(destination.join(FILE))?)?;
    let mut movie = reading.movie;
    let end_ms = movie
        .metadata
        .origin_ms
        .saturating_add(u64::try_from(movie.prefix.duration().as_millis())?)
        .min(u64::try_from(RECORDING_LIMIT.as_millis())?);
    let partial = movie.prefix.truncated()
        || movie
            .metadata
            .finished
            .as_ref()
            .is_none_or(|finish| !finish.finalized);
    movie.metadata.coverage = Some(Coverage {
        end_ms,
        frames: movie.prefix.frames(),
        bytes: movie.prefix.bytes(),
        annotations_through_ms: annotations.as_millis(),
        partial,
    });
    movie.metadata.replace(destination)?;
    Ok(movie.metadata)
}

pub(crate) fn code(path: &Path) -> String {
    let text = path.display().to_string();
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.saturating_add(1));
    format!("{fence} {text} {fence}")
}

#[cfg(test)]
pub(in crate::show) mod tests;
