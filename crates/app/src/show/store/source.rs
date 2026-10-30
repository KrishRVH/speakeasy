//! Private source sidecars: bounded index reads, range-based frame loading, and streamed writes.

use std::{
    fs::{self, File},
    io::{self, BufWriter, Read as _, Seek as _, Write as _},
    ops::Range,
    path::Path,
};

use anyhow::Context as _;
use speakeasy_show::{Journal, Moment, Source, SourceFrame};

use super::recall;

pub(crate) const FILE: &str = "source.frames";

/// Includes the uncovered time before the first sample and after the final sample.
pub(crate) fn longest_gap(source: &Source) -> u64 {
    source
        .moments()
        .iter()
        .copied()
        .chain(std::iter::once(source.duration()))
        .scan(Moment::START, |previous, at| {
            let gap = at.as_millis().saturating_sub(previous.as_millis());
            *previous = at;
            Some(gap)
        })
        .max()
        .unwrap_or(0)
}

/// Source is optional: old bundles and simulated bundles need not contain it. A present linked,
/// nonregular, or oversized sidecar is an error, never treated as absent.
pub(crate) fn size(directory: &Path) -> io::Result<Option<u64>> {
    match recall::regular_file(directory, FILE, Source::FILE_LIMIT) {
        Ok(()) => fs::metadata(directory.join(FILE)).map(|metadata| Some(metadata.len())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn inspect(directory: &Path) -> anyhow::Result<Source> {
    let mut file = open(directory)?;
    Source::read(file.metadata()?.len(), |range| read_range(&mut file, range))
        .context("Cannot inspect source.frames")
}

/// A selected index is an immutable snapshot. The underlying file is opened only for this read,
/// so keeping a viewer open does not prevent expiry on Windows.
pub(crate) fn frame(directory: &Path, source: &Source, at: Moment) -> anyhow::Result<SourceFrame> {
    let mut file = open(directory)?;
    anyhow::ensure!(
        file.metadata()?.len() == source.bytes(),
        "Source frames changed. Refresh the Show before reading them."
    );
    source
        .frame(at, |range| read_range(&mut file, range))
        .context("Cannot read the recorded source frame")
}

fn open(directory: &Path) -> anyhow::Result<File> {
    recall::regular_file(directory, FILE, Source::FILE_LIMIT)
        .context("Source frames are unavailable for this Show")?;
    File::open(directory.join(FILE)).context("Cannot open source.frames")
}

fn read_range(file: &mut File, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
    let length = range
        .end
        .checked_sub(range.start)
        .filter(|length| *length <= Source::FILE_LIMIT)
        .context("Source byte range is invalid")?;
    let mut bytes = vec![0; usize::try_from(length)?];
    file.seek(io::SeekFrom::Start(range.start))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn write(directory: &Path, journal: &Journal, duration: Moment) -> anyhow::Result<()> {
    let mut staging = tempfile::NamedTempFile::new_in(directory)?;
    {
        let mut output = BufWriter::new(staging.as_file_mut());
        journal.write_source(duration, |bytes| {
            output.write_all(bytes).map_err(anyhow::Error::from)
        })?;
        output.flush()?;
        output.get_ref().sync_all()?;
    }
    staging
        .persist_noclobber(directory.join(FILE))
        .map_err(|error| error.error)?;
    #[cfg(unix)]
    {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The complete sidecar is already visible; a directory sync failure must not report it as lost"
        )]
        let _ = File::open(directory).and_then(|file| file.sync_all());
    }
    Ok(())
}

/// Writes only to a newly created explicit destination. Neither an existing export nor a link is
/// replaced. A failed write removes only the new file this call owns.
pub(crate) fn export(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        // Keep the original failure, but make a failed cleanup visible as well.
        return match fs::remove_file(path) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(io::Error::other(format!(
                "The export could not be written ({error}) or removed ({cleanup}); remove {}",
                path.display()
            ))),
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests;
