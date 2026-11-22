use std::{
    fs::{self, File},
    io::{self, BufWriter, Read as _, Seek as _, Write as _},
    ops::Range,
    path::Path,
};

use anyhow::Context as _;
use speakeasy_show::{Journal, Moment, Source, SourceFrame};

pub(crate) const FILE: &str = "source.frames";

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

pub(crate) fn present(directory: &Path) -> io::Result<bool> {
    fs::exists(directory.join(FILE))
}

pub(crate) fn inspect(directory: &Path) -> anyhow::Result<Source> {
    Reader::open(directory)?.inspect()
}

pub(crate) fn frame(directory: &Path, source: &Source, at: Moment) -> anyhow::Result<SourceFrame> {
    Reader::open(directory)?.frame(source, at)
}

pub(in crate::show) struct Reader(File);

impl Reader {
    pub(in crate::show) fn open(directory: &Path) -> anyhow::Result<Self> {
        File::open(directory.join(FILE))
            .map(Self)
            .context("Source frames are unavailable for this Show")
    }

    pub(in crate::show) fn inspect(&mut self) -> anyhow::Result<Source> {
        Source::read(self.0.metadata()?.len(), |range| {
            read_range(&mut self.0, range)
        })
        .context("Cannot inspect source.frames")
    }

    pub(in crate::show) fn frame(
        &mut self,
        source: &Source,
        at: Moment,
    ) -> anyhow::Result<SourceFrame> {
        source
            .frame(at, |range| read_range(&mut self.0, range))
            .context("Cannot read the recorded source frame")
    }
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
    let mut output = BufWriter::new(staging.as_file_mut());
    journal.write_source(duration, |bytes| {
        output.write_all(bytes).map_err(anyhow::Error::from)
    })?;
    output.flush()?;
    drop(output);
    staging
        .persist_noclobber(directory.join(FILE))
        .map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn export(path: &Path, bytes: &[u8]) -> io::Result<()> {
    File::create_new(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests;
