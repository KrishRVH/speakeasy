use std::{
    fs::File,
    io::{self, Read as _, Seek, Write as _},
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use serde::Serialize;
use speakeasy_show::{Bundle, Manifest, Source};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::{movie, recall, source};

const GUIDE: &str = "README.md";

const CONTENT_LIMIT: u64 = 2 << 30;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Exported {
    pub path: PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub retained_frames: bool,
}

pub(crate) fn write(directory: &Path, destination: &Path) -> anyhow::Result<Exported> {
    let target = Target::new(directory, destination, "ZIP")?;
    let ((files, retained_frames), bytes) = target.publish(".speakeasy-export-", |staging| {
        archive(&target.show, staging.as_file_mut())
    })?;
    Ok(Exported {
        path: target.path,
        files,
        bytes,
        retained_frames,
    })
}

pub(super) struct Target {
    pub show: PathBuf,
    pub path: PathBuf,
    folder: PathBuf,
    kind: &'static str,
}

impl Target {
    pub(super) fn new(show: &Path, destination: &Path, kind: &'static str) -> anyhow::Result<Self> {
        let show = show
            .canonicalize()
            .context("Cannot open the Show to export")?;
        let path = std::path::absolute(destination)?;
        anyhow::ensure!(
            path.symlink_metadata()
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound),
            "The export path exists or cannot be inspected; choose a new {kind} file"
        );
        let folder = path
            .parent()
            .with_context(|| format!("Choose a new {kind} file inside a folder"))?
            .to_owned();
        anyhow::ensure!(
            !folder
                .canonicalize()
                .context("Cannot open the export folder; choose an existing folder")?
                .starts_with(&show),
            "Choose an export location outside the original Show"
        );
        Ok(Self {
            show,
            path,
            folder,
            kind,
        })
    }

    pub(super) fn publish<T>(
        &self,
        prefix: &str,
        write: impl FnOnce(&mut tempfile::NamedTempFile) -> anyhow::Result<T>,
    ) -> anyhow::Result<(T, u64)> {
        let mut staging = tempfile::Builder::new()
            .prefix(prefix)
            .tempfile_in(&self.folder)
            .context("Cannot create the export; check the destination folder and free space")?;
        let written = write(&mut staging).and_then(|value| {
            staging
                .as_file()
                .sync_all()
                .context("Cannot flush the export")?;
            Ok((value, staging.as_file().metadata()?.len()))
        });
        let written = match written {
            Ok(written) => written,
            Err(error) => return Err(failed(staging, error)),
        };
        match staging.persist_noclobber(&self.path) {
            Ok(_published) => Ok(written),
            Err(error) => Err(failed(
                error.file,
                anyhow::Error::from(error.error).context(format!(
                    "Cannot save the export; choose a new {} path and check free space",
                    self.kind
                )),
            )),
        }
    }
}

fn failed(staging: tempfile::NamedTempFile, error: anyhow::Error) -> anyhow::Error {
    let path = staging.path().to_owned();
    match staging.close() {
        Ok(()) => error,
        Err(cleanup) => error.context(format!(
            "Remove the unfinished export at {}: {cleanup}",
            path.display()
        )),
    }
}

fn archive(
    directory: &Path,
    writer: &mut (impl io::Write + Seek),
) -> anyhow::Result<(usize, bool)> {
    let bytes = recall::read_file(directory, Manifest::FILE, recall::MANIFEST_LIMIT)
        .context("Cannot export show.json")?;
    let manifest = Manifest::parse(&bytes)?;
    let mut archive = ZipWriter::new(writer);

    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o600);
    let mut remaining = CONTENT_LIMIT;
    let mut files = 0_usize;
    for name in manifest.files() {
        archive.start_file(&name, options)?;
        if name == Manifest::FILE {
            archive.write_all(&bytes)?;
            remaining = remaining.saturating_sub(u64::try_from(bytes.len())?);
        } else {
            let limit = if name == Bundle::SUMMARY {
                recall::SUMMARY_LIMIT
            } else {
                recall::IMAGE_LIMIT
            };
            let copied = copy(directory, &name, limit.min(remaining), &mut archive)
                .with_context(|| format!("Cannot export {name}"))?;
            remaining = remaining.saturating_sub(copied);
        }
        files = files.saturating_add(1);
    }
    let retained_frames = source::size(directory)?.is_some();
    if retained_frames {
        let index = source::inspect(directory).context("Cannot export damaged retained frames")?;
        anyhow::ensure!(
            index.duration() == manifest.recording().duration,
            "Source and Show timelines differ; inspect the bundle before exporting"
        );
        archive.start_file(source::FILE, options)?;
        let copied = copy(
            directory,
            source::FILE,
            Source::FILE_LIMIT.min(remaining),
            &mut archive,
        )
        .context("Cannot export retained frames")?;
        remaining = remaining.saturating_sub(copied);
        files = files.saturating_add(1);
    }
    let mut movie = movie::Reading::optional(directory, manifest.recording().duration)
        .context("Cannot export damaged video")?;
    if let Some(reading) = &mut movie {
        archive.start_file(movie::FILE, options)?;
        let copied = reading
            .copy_to(&mut archive, remaining)
            .context("Cannot export video")?;
        remaining = remaining.saturating_sub(copied);
        let metadata = serde_json::to_vec(&reading.movie.metadata)?;
        anyhow::ensure!(
            u64::try_from(metadata.len())? <= remaining,
            "Movie timing exceeds the export size limit"
        );
        archive.start_file(movie::HEADER, options)?;
        archive.write_all(&metadata)?;
        files = files.saturating_add(2);
    }
    archive.start_file(GUIDE, options)?;
    guide(
        &manifest,
        retained_frames,
        movie.as_ref().map(|reading| &reading.movie.metadata),
        &mut archive,
    )?;
    archive.finish()?.flush()?;
    Ok((files.saturating_add(1), retained_frames))
}

fn copy(directory: &Path, name: &str, limit: u64, out: &mut impl io::Write) -> io::Result<u64> {
    recall::regular_file(directory, name, limit)?;
    let file = File::open(directory.join(name.split('/').collect::<PathBuf>()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(io::Error::other(
            "file changed or exceeds the export size limit",
        ));
    }
    let copied = io::copy(&mut file.take(limit.saturating_add(1)), out)?;
    if copied != metadata.len() || copied > limit {
        return Err(io::Error::other("file changed during export; try again"));
    }
    Ok(copied)
}

fn guide(
    manifest: &Manifest,
    retained_frames: bool,
    movie: Option<&movie::Metadata>,
    out: &mut impl io::Write,
) -> io::Result<()> {
    writeln!(out, "# Speakeasy Show\n")?;
    writeln!(
        out,
        "Unzip this archive before opening its files. Start with [the saved summary](summary.md), then open the contact sheets below. The summary is preserved exactly as saved, including your edits; any machine-local paths in it may be stale. Use this guide's relative links on this machine.\n"
    )?;
    writeln!(
        out,
        "[show.json](show.json) contains recorded narration, timings, marks, and frame metadata. Image files keep their saved pixels and annotations. This export contains no recovery audio. Nothing is uploaded by exporting.\n"
    )?;
    writeln!(
        out,
        "This copy does not expire automatically. Keeping or deleting the original Show does not change this archive or extracted copies.\n\n## Contact sheets\n"
    )?;
    for (index, (name, _)) in manifest.sheets().enumerate() {
        writeln!(out, "- [Contact sheet {}]({name})", index.saturating_add(1))?;
    }
    writeln!(out, "\n## Keyframes\n")?;
    for frame in manifest.keyframes() {
        writeln!(
            out,
            "- [Frame {} at {} ms]({})",
            frame.index,
            frame.at.as_millis(),
            frame.file()
        )?;
    }
    if retained_frames {
        writeln!(
            out,
            "\n## Retained frames\n\n[source.frames](source.frames) contains the additional saved samples. With Speakeasy installed, run `speakeasy show frames . --json` inside this extracted folder to inspect sample times, or `speakeasy show frame . --at 0:01 --out ../frame.png` to export a sample. Use this folder's path, not an ID from the original machine."
        )?;
    }
    if let Some(movie) = movie {
        writeln!(
            out,
            "\n## Video\n\nOpen [recording.mp4](recording.mp4) in a video player. [recording.json](recording.json) records its session origin and surviving coverage.{}",
            movie
                .note(Path::new(movie::FILE))
                .map_err(io::Error::other)?
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
