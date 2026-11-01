mod encode;
mod timeline;

use std::{
    fs::File,
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use serde::Serialize;
use speakeasy_show::{Annotations, Manifest, Span};

use super::{movie, recall, voice};

#[derive(Clone, Copy, Default)]
pub(crate) struct Options {
    pub span: Option<Span>,
    pub captions: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Exported {
    pub path: PathBuf,
    pub bytes: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub frames: u64,
    pub voice: voice::Status,
    pub captions: usize,
    pub frames_without_annotations: u64,
    pub omitted_annotations: usize,
}

impl Exported {
    pub(crate) fn note(&self) -> String {
        let voice = match self.voice {
            voice::Status::Available => "with original voice",
            voice::Status::Incomplete => "with partial voice (recording was interrupted)",
            voice::Status::Absent => "silent (this Show has no saved voice)",
            voice::Status::Expired => "silent (voice expired after seven days)",
        };
        let annotations = if self.frames_without_annotations > 0 || self.omitted_annotations > 0 {
            " Some annotations could not be placed; see the saved Show for full context."
        } else {
            ""
        };
        format!(
            "Exported {} {voice}, with {} caption cues.{annotations} This copy does not expire automatically.",
            self.path.display(),
            self.captions
        )
    }
}

pub(crate) fn write(
    directory: &Path,
    destination: &Path,
    options: Options,
    cancelled: impl Fn() -> bool + Sync,
) -> anyhow::Result<Exported> {
    write_with(
        directory,
        destination,
        options,
        cancelled,
        super::private_directory,
    )
}

fn write_with(
    directory: &Path,
    destination: &Path,
    options: Options,
    cancelled: impl Fn() -> bool + Sync,
    scratch: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Exported> {
    let directory = directory
        .canonicalize()
        .context("Cannot open the Show to export")?;
    let destination = std::path::absolute(destination)?;
    anyhow::ensure!(
        destination
            .symlink_metadata()
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound),
        "The export path exists or cannot be inspected; choose a new MP4 file"
    );
    let parent = destination
        .parent()
        .context("Choose an MP4 file inside a folder")?;
    anyhow::ensure!(
        !parent.canonicalize()?.starts_with(&directory),
        "Choose an export location outside the original Show"
    );
    anyhow::ensure!(!cancelled(), "Video export cancelled");
    let manifest = Manifest::parse(&recall::read_file(
        &directory,
        Manifest::FILE,
        recall::MANIFEST_LIMIT,
    )?)?;
    let mut reading = movie::Reading::open(&directory, manifest.recording().duration)?;
    let voice = voice::read(&directory, SystemTime::now())?;
    let audio = voice.audio();
    let timing = timeline::Timing::new(&reading.movie, options.span, audio)?;
    let scene = Annotations::new(manifest.recording(), manifest.marks())?;
    let work = super::staging(&scratch()?).context(
        "Cannot create private video scratch space; check the Shows folder's space and permissions",
    )?;
    let mut source = File::create_new(work.path().join("source.mp4"))?;
    reading.copy_to(&mut source, speakeasy_show::MoviePrefix::FILE_LIMIT)?;
    source.flush()?;
    anyhow::ensure!(!cancelled(), "Video export cancelled");
    let rate = audio.map(|audio| audio.rate);
    if let Some(audio) = audio {
        timeline::audio(
            audio,
            &timing,
            &mut File::create_new(work.path().join("voice.pcm"))?,
        )?;
    }
    let voice_status = voice.status();
    drop(voice);
    let captions = if options.captions {
        timeline::captions(
            manifest.recording(),
            &timing,
            &mut File::create_new(work.path().join("captions.srt"))?,
        )?
    } else {
        0
    };
    let mut staging = tempfile::Builder::new()
        .prefix(".speakeasy-video-")
        .tempfile_in(parent)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let stats = runtime.block_on(encode::run(
        encode::Inputs {
            work: work.path(),
            output: staging.path(),
            movie: &reading.movie,
            scene: &scene,
            timing: &timing,
            rate,
            captions: captions > 0,
        },
        &cancelled,
    ))?;
    anyhow::ensure!(!cancelled(), "Video export cancelled");
    staging.as_file_mut().flush()?;
    staging.as_file().sync_all()?;
    let bytes = staging.as_file().metadata()?.len();
    let _published = staging
        .persist_noclobber(&destination)
        .map_err(|error| error.error)
        .context("Cannot publish the video; choose a new MP4 path and check free space")?;
    Ok(Exported {
        path: destination,
        bytes,
        start_ms: timing.start_ms,
        end_ms: timing.end_ms,
        frames: timing.frames,
        voice: voice_status,
        captions,
        frames_without_annotations: stats.unpainted,
        omitted_annotations: stats.omitted,
    })
}

fn seconds(duration: Duration) -> String {
    format!("{}.{:09}", duration.as_secs(), duration.subsec_nanos())
}

#[cfg(test)]
mod tests;
