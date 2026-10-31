//! Silent movie storage and original-session timing. The container supplies surviving coverage;
//! checkpoint metadata supplies only its origin, completion receipt, and annotation horizon.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Seek as _, Write as _},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use speakeasy_show::{Moment, MoviePrefix};

use super::{checkpoint, recall};

pub(crate) const FILE: &str = "recording.mp4";
pub(crate) const HEADER: &str = "recording.json";
pub(super) const HEADER_LIMIT: u64 = 4 << 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Metadata {
    version: u8,
    pub origin_ms: u64,
    pub size: [u32; 2],
    pub finished: Option<Finished>,
    /// Filled during publication; absent in a live checkpoint.
    pub coverage: Option<Coverage>,
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
    ) -> anyhow::Result<Self> {
        let origin = at
            .checked_duration_since(started)
            .context("Movie began before its session")?;
        anyhow::ensure!(
            origin < speakeasy_core::gesture::RECORDING_LIMIT,
            "Movie began after the recording limit"
        );
        anyhow::ensure!(
            size.into_iter()
                .all(|edge| (2..=speakeasy_show::MAX_FRAME_EDGE).contains(&edge)
                    && edge.is_multiple_of(2)),
            "Movie canvas is invalid"
        );
        let metadata = Self {
            version: 1,
            origin_ms: u64::try_from(origin.as_millis())?,
            size,
            finished: None,
            coverage: None,
        };
        let bytes = serde_json::to_vec(&metadata)?;
        let mut file = File::create_new(directory.join(HEADER))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        checkpoint::sync_directory(directory)?;
        Ok(metadata)
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
                .min(speakeasy_core::gesture::RECORDING_LIMIT)
                .as_millis(),
        )?;
        anyhow::ensure!(
            last_ms >= self.origin_ms && last_ms <= through_ms && submitted <= 9_001,
            "Movie completion is invalid"
        );
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
        staging.as_file().sync_all()?;
        staging
            .persist(directory.join(HEADER))
            .map_err(|error| error.error)?;
        checkpoint::sync_directory(directory)?;
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
        Ok(format!(
            "\n\nSilent video: `{}`. Recorded session coverage: {}–{} ms. {partial}The video has no narration audio or drawn annotations; its last image may be held through the end. Marks and narration in show.json are documented through {} ms.\n",
            path.display(),
            self.origin_ms,
            coverage.end_ms,
            coverage.annotations_through_ms
        ))
    }
}

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
        let reading = open(directory)?
            .context("This Show has no retained video; use its source frames instead")?;
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
        Ok(reading)
    }

    pub(crate) fn frame(
        self,
        at: Moment,
        cancelled: impl Fn() -> bool,
    ) -> anyhow::Result<Option<speakeasy_show::SourceFrame>> {
        let metadata = &self.movie.metadata;
        let coverage = metadata
            .coverage
            .as_ref()
            .context("Movie has no published coverage")?;
        anyhow::ensure!(
            (metadata.origin_ms..=coverage.end_ms).contains(&at.as_millis()),
            "Requested time is outside the retained video coverage"
        );
        let requested = Duration::from_millis(at.as_millis().saturating_sub(metadata.origin_ms));
        let Some(frame) = speakeasy_platform::read_movie_frame(
            self.file,
            self.movie.prefix.bytes,
            metadata.size,
            requested,
            cancelled,
        )?
        else {
            return Ok(None);
        };
        anyhow::ensure!(
            frame.at < self.movie.prefix.duration,
            "Decoded video sample exceeds its retained coverage"
        );
        Ok(Some(speakeasy_show::SourceFrame {
            at: Moment::from_millis(
                metadata
                    .origin_ms
                    .saturating_add(u64::try_from(frame.at.as_millis())?),
            ),
            image: speakeasy_show::Image::from_bgra(frame.size[0], frame.size[1], frame.bgra)?,
        }))
    }
}

pub(super) enum CheckpointMovie {
    Absent,
    Unavailable,
    Present(Inspected),
}

pub(super) fn checkpoint(directory: &Path) -> anyhow::Result<CheckpointMovie> {
    let absent = match fs::symlink_metadata(directory.join(FILE)) {
        Ok(metadata) => metadata.is_file() && metadata.len() == 0,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    if absent {
        match fs::symlink_metadata(directory.join(HEADER)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CheckpointMovie::Absent);
            },
            Err(error) => return Err(error.into()),
            Ok(metadata) => anyhow::ensure!(
                metadata.is_file() && metadata.len() <= HEADER_LIMIT,
                "Movie timing must be a regular bounded file"
            ),
        }
        return Ok(CheckpointMovie::Unavailable);
    }
    match inspect(directory) {
        Ok(Some(movie)) => Ok(CheckpointMovie::Present(movie)),
        Ok(None) => Ok(CheckpointMovie::Absent),
        Err(error) if matches!(error.downcast_ref::<speakeasy_show::MovieError>(), Some(speakeasy_show::MovieError::NoFragment)) => Ok(CheckpointMovie::Unavailable),
        Err(error) => Err(error.context(format!("Video cannot be recovered automatically. To save the stills, move recording.mp4 and recording.json out of {} into a private folder, then choose Recover again. Keep those files for repair.", directory.display()))),
    }
}

fn read_metadata(directory: &Path) -> anyhow::Result<Metadata> {
    let bytes = recall::read_file(directory, HEADER, HEADER_LIMIT)?;
    let metadata: Metadata = serde_json::from_slice(&bytes)
        .map_err(|_private_content| anyhow::anyhow!("Movie timing metadata is damaged"))?;
    anyhow::ensure!(
        metadata.version == 1
            && metadata.origin_ms
                < u64::try_from(speakeasy_core::gesture::RECORDING_LIMIT.as_millis())?,
        "Movie timing metadata is unsupported"
    );
    Ok(metadata)
}

/// Missing video remains compatible with still-only bundles. A present damaged or linked movie
/// is an error, so exports and publication never silently substitute a different evidence set.
pub(crate) fn inspect(directory: &Path) -> anyhow::Result<Option<Inspected>> {
    Ok(open(directory)?.map(|reading| reading.movie))
}

fn open(directory: &Path) -> anyhow::Result<Option<Reading>> {
    match fs::symlink_metadata(directory.join(FILE)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match fs::symlink_metadata(directory.join(HEADER)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
                Ok(_) => Err(anyhow::anyhow!("Movie timing exists without its video")),
            };
        },
        Err(error) => return Err(error.into()),
        Ok(_) => {},
    }
    recall::regular_file(directory, FILE, MoviePrefix::FILE_LIMIT)?;
    let metadata = read_metadata(directory)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // Deny writes through validation and native decoding; expiry may still unlink the bundle.
        const SHARE_READ_DELETE: u32 = 0x1 | 0x4;
        options.share_mode(SHARE_READ_DELETE);
    }
    let mut file = options.open(directory.join(FILE))?;
    let length = file.metadata()?.len();
    let prefix = MoviePrefix::read(length, |range| {
        let length = range
            .end
            .checked_sub(range.start)
            .context("Movie metadata range is invalid")?;
        let mut bytes = vec![0; usize::try_from(length)?];
        file.seek(std::io::SeekFrom::Start(range.start))?;
        file.read_exact(&mut bytes)?;
        anyhow::Ok(bytes)
    })?;
    anyhow::ensure!(
        prefix.size == metadata.size,
        "Movie canvas differs from its timing metadata"
    );
    let limit = u64::try_from(speakeasy_core::gesture::RECORDING_LIMIT.as_millis())?;
    let end_ms = metadata
        .origin_ms
        .saturating_add(u64::try_from(prefix.duration.as_millis())?);
    // A codec's final sample occupies up to one 30 Hz tick beyond the requested end.
    anyhow::ensure!(
        end_ms <= limit.saturating_add(34),
        "Movie exceeds the session recording limit"
    );
    if let Some(finished) = &metadata.finished {
        anyhow::ensure!(
            finished.submitted > 0
                && (!finished.finalized || end_ms <= finished.through_ms.saturating_add(34)),
            "Movie fragments exceed the acknowledged capture end"
        );
        anyhow::ensure!(
            finished.last_ms >= metadata.origin_ms,
            "Movie completion precedes its origin"
        );
        anyhow::ensure!(
            finished.last_ms <= finished.through_ms
                && finished.through_ms <= limit
                && finished.submitted <= 9_001,
            "Movie completion metadata is invalid"
        );
    }
    if let Some(coverage) = &metadata.coverage {
        anyhow::ensure!(
            coverage.partial
                || metadata
                    .finished
                    .as_ref()
                    .is_some_and(|finish| finish.finalized),
            "Movie has no finalized completion receipt"
        );
        anyhow::ensure!(
            coverage.bytes == prefix.bytes
                && length == prefix.bytes
                && !prefix.truncated
                && coverage.frames == prefix.frames
                && coverage.end_ms == end_ms.min(limit)
                && coverage.annotations_through_ms <= limit,
            "Saved movie coverage differs from its fragments"
        );
    }
    Ok(Some(Reading {
        file,
        movie: Inspected { metadata, prefix },
    }))
}

pub(crate) fn saved(directory: &Path, annotations: Moment) -> anyhow::Result<Option<Metadata>> {
    let Some(movie) = inspect(directory)? else {
        return Ok(None);
    };
    let coverage = movie
        .metadata
        .coverage
        .as_ref()
        .context("Movie has no published coverage")?;
    anyhow::ensure!(
        coverage.annotations_through_ms == annotations.as_millis(),
        "Movie annotation coverage differs from the Show"
    );
    Ok(Some(movie.metadata))
}

/// The source checkpoint stays locked throughout this operation. A valid movie's copy/sync
/// failure aborts publication; its only surviving copy must remain recoverable.
pub(in crate::show::store) fn retain(
    source: &Path,
    destination: &Path,
    mut movie: Inspected,
    annotations: Moment,
) -> anyhow::Result<Metadata> {
    let input = File::open(source.join(FILE))?;
    let mut output = File::create_new(destination.join(FILE))?;
    let copied = std::io::copy(&mut input.take(movie.prefix.bytes), &mut output)?;
    anyhow::ensure!(
        copied == movie.prefix.bytes,
        "Movie changed during publication"
    );
    output.sync_all()?;
    let end_ms = movie
        .metadata
        .origin_ms
        .saturating_add(u64::try_from(movie.prefix.duration.as_millis())?)
        .min(u64::try_from(
            speakeasy_core::gesture::RECORDING_LIMIT.as_millis(),
        )?);
    let partial = movie.prefix.truncated
        || movie
            .metadata
            .finished
            .as_ref()
            .is_none_or(|finish| !finish.finalized);
    movie.metadata.coverage = Some(Coverage {
        end_ms,
        frames: movie.prefix.frames,
        bytes: movie.prefix.bytes,
        annotations_through_ms: annotations.as_millis(),
        partial,
    });
    movie.metadata.replace(destination)?;
    Ok(movie.metadata)
}

#[cfg(test)]
pub(in crate::show) mod tests;
