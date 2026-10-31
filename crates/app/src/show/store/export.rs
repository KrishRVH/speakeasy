//! Explicit portable ZIP exports. Only format-owned files leave the bundle; summary edits remain
//! verbatim, and a relative-path guide replaces reliance on the recording machine's paths.

use std::{
    fs::File,
    io::{self, Read as _, Seek, Write as _},
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use serde::Serialize;
use speakeasy_show::{Bundle, Manifest, Source};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::{recall, source};

const GUIDE: &str = "README.md";
/// A malicious or damaged inventory cannot make an export consume unbounded disk space.
const CONTENT_LIMIT: u64 = 1 << 30;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Exported {
    pub path: PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub retained_frames: bool,
}

/// Writes an explicit new archive without changing the original Show or its retention. Staging
/// is private and removed on failure; the destination appears only after the archive is flushed.
/// Exports are independent copies and do not expire with the managed Show.
pub(crate) fn write(directory: &Path, destination: &Path) -> anyhow::Result<Exported> {
    let directory = directory
        .canonicalize()
        .context("Cannot open the Show to export")?;
    let destination = std::path::absolute(destination)?;
    anyhow::ensure!(
        destination
            .symlink_metadata()
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound),
        "The export path exists or cannot be inspected; choose a new ZIP file"
    );
    let parent = destination
        .parent()
        .context("Choose a ZIP file inside a folder")?;
    anyhow::ensure!(
        !parent.canonicalize()?.starts_with(&directory),
        "Choose an export location outside the original Show"
    );
    let mut staging = tempfile::Builder::new()
        .prefix(".speakeasy-export-")
        .tempfile_in(parent)
        .context("Cannot create the export; check the destination folder and free space")?;
    let result = archive(&directory, staging.as_file_mut()).and_then(|(files, retained)| {
        staging
            .as_file()
            .sync_all()
            .context("Cannot flush the Show export")?;
        Ok((files, retained, staging.as_file().metadata()?.len()))
    });
    let (files, retained_frames, bytes) = match result {
        Ok(result) => result,
        Err(error) => return Err(failed(staging, error)),
    };
    let _published = match staging.persist_noclobber(&destination) {
        Ok(file) => file,
        Err(error) => {
            return Err(failed(
                error.file,
                anyhow::Error::from(error.error)
                    .context("Cannot save the export; choose a new ZIP path and check free space"),
            ));
        },
    };
    Ok(Exported {
        path: destination,
        files,
        bytes,
        retained_frames,
    })
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
    // Images and the source journal are already compressed. Stored entries also work in native
    // archive readers without adding codec dependencies or recompressing screen pixels.
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
        copy(
            directory,
            source::FILE,
            Source::FILE_LIMIT.min(remaining),
            &mut archive,
        )
        .context("Cannot export retained frames")?;
        files = files.saturating_add(1);
    }
    archive.start_file(GUIDE, options)?;
    guide(&manifest, retained_frames, &mut archive)?;
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

fn guide(manifest: &Manifest, retained_frames: bool, out: &mut impl io::Write) -> io::Result<()> {
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
    Ok(())
}

#[cfg(test)]
mod tests;
