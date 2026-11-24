mod encode;
mod timeline;

use std::{
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use serde::Serialize;
use speakeasy_show::{Annotations, Manifest, Span};

use super::{export, movie, voice};

pub(crate) const CANCELLED: &str = "Video export cancelled";

#[derive(Clone, Copy, Default)]
pub(crate) struct Options {
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
            voice::Status::Available => "with original audio",
            voice::Status::Incomplete => "with partial audio (recording was interrupted)",
            voice::Status::Absent => "silent (this Show has no saved audio)",
        };
        let annotations = if self.frames_without_annotations > 0 || self.omitted_annotations > 0 {
            " Some annotations could not be placed; see the saved Show for full context."
        } else {
            ""
        };
        format!(
            "Exported {} {voice}, with {} caption cue{}.{annotations} This copy does not expire automatically.",
            self.path.display(),
            self.captions,
            if self.captions == 1 { "" } else { "s" }
        )
    }
}

pub(crate) fn write(
    directory: &Path,
    destination: &Path,
    options: Options,
    clip: impl FnOnce(&Manifest) -> anyhow::Result<Option<Span>>,
    cancelled: impl Fn() -> bool + Sync,
) -> anyhow::Result<Exported> {
    let target = export::Target::new(directory, destination, "MP4")?;
    anyhow::ensure!(!cancelled(), CANCELLED);
    let directory = &target.show;
    let manifest = Manifest::parse(&std::fs::read(directory.join(Manifest::FILE))?)?;
    let span = clip(&manifest)?;
    let mut reading = movie::Reading::open(directory, manifest.recording().duration)?;
    let voice = voice::read(directory)?;
    let audio = voice.as_ref();
    let timing = timeline::Timing::new(&reading.movie, span, audio)?;
    let scene = Annotations::new(manifest.recording(), manifest.marks())?;
    let work = tempfile::Builder::new()
        .prefix(".speakeasy-video-")
        .tempdir_in(&target.folder)
        .context("Can't create the video export; check the destination folder and free space")?;
    let mut source = File::create_new(work.path().join(encode::SOURCE))?;
    reading.copy_to(&mut source)?;
    anyhow::ensure!(!cancelled(), CANCELLED);
    let rate = audio.map(|audio| audio.rate);
    if let Some(audio) = audio {
        timeline::audio(
            audio,
            &timing,
            &mut File::create_new(work.path().join(encode::VOICE))?,
        )?;
    }
    let voice_status = voice::Status::of(audio);
    drop(voice);
    let captions = if options.captions {
        timeline::captions(
            manifest.recording(),
            &timing,
            &mut File::create_new(work.path().join(encode::CAPTIONS))?,
        )?
    } else {
        0
    };
    let (stats, bytes) = target.publish(".speakeasy-video-", |staging| {
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
        anyhow::ensure!(!cancelled(), CANCELLED);
        Ok(stats)
    })?;
    Ok(Exported {
        path: target.path,
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

#[cfg(test)]
mod tests;
