mod buffers;
mod output;
mod settings;
#[cfg(test)]
mod tests;

use std::{
    ops::ControlFlow,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, ensure};
use block2::RcBlock;
use objc2::{AllocAnyThread as _, rc::Retained, runtime::ProtocolObject};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterStatus, AVFileTypeMPEG4,
    AVFileTypeProfileMPEG4AppleHLS, AVMediaTypeVideo,
};
use objc2_core_media::CMTime;
use objc2_uniform_type_identifiers::UTType;

use crate::movie::encode;

pub(crate) struct Codec {
    native: Retained<AVAssetWriter>,
    input: Retained<AVAssetWriterInput>,
    /// The writer's delegate and owner of the output file; the writer holds it weakly.
    ///
    /// Kept until finalization completes, since segments arrive through it until then.
    output: Retained<output::Output>,
    buffers: buffers::Buffers,
    frames: u32,
    finished: bool,
}

impl encode::Codec for Codec {
    const CLOCK: encode::Clock = encode::Clock::Nanoseconds;

    fn open(path: &Path, size: [u32; 2], limit: u64) -> anyhow::Result<Self> {
        let output = output::Output::new(path, limit)?;
        let buffers = buffers::Buffers::new(size)?;
        let settings = settings::video(size)?;

        let (native, input) = unsafe {
            let kind = UTType::typeWithIdentifier(AVFileTypeMPEG4.context("MP4 is unavailable")?)
                .context("MP4 type is unavailable")?;
            // With a content type and no URL, the writer hands every fragment to the delegate.
            let native = AVAssetWriter::initWithContentType(AVAssetWriter::alloc(), &kind);
            native.setDelegate(Some(ProtocolObject::from_ref(&*output)));
            native.setOutputFileTypeProfile(AVFileTypeProfileMPEG4AppleHLS);
            native.setPreferredOutputSegmentInterval(CMTime::new(1, 1));
            native.setInitialSegmentStartTime(CMTime::new(0, 1));
            let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                AVMediaTypeVideo.context("Video type is unavailable")?,
                Some(&settings),
            );
            input.setExpectsMediaDataInRealTime(true);
            input.setMediaTimeScale(30_000);
            ensure!(
                native.canAddInput(&input),
                "The native movie format is unavailable"
            );
            native.addInput(&input);
            ensure!(
                native.startWriting(),
                "The native movie writer couldn't start"
            );
            native.startSessionAtSourceTime(CMTime::new(0, 1));
            (native, input)
        };
        Ok(Self {
            native,
            input,
            output,
            buffers,
            frames: 0,
            finished: false,
        })
    }

    fn write(
        &mut self,
        bgra: &[u8],
        index: u32,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<ControlFlow<()>> {
        let began = Instant::now();
        loop {
            if cancelled() {
                return Ok(ControlFlow::Break(()));
            }
            self.output.check()?;

            unsafe {
                ensure!(
                    self.native.status() == AVAssetWriterStatus::Writing,
                    "The native movie writer stopped"
                );
                if self.input.isReadyForMoreMediaData() {
                    break;
                }
            }
            ensure!(
                began.elapsed() < Duration::from_secs(2),
                "The native movie writer fell behind"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let sample = self.buffers.sample(bgra, index)?;
        if cancelled() {
            return Ok(ControlFlow::Break(()));
        }

        let appended = unsafe { self.input.appendSampleBuffer(&sample) };
        ensure!(appended, "The native movie writer refused a frame");
        self.frames = self
            .frames
            .checked_add(1)
            .context("Movie sample count overflowed")?;
        Ok(ControlFlow::Continue(()))
    }

    fn finish(mut self) -> anyhow::Result<()> {
        ensure!(self.frames > 0, "The movie contains no frames");
        self.output.check()?;
        let (completed, reply) = mpsc::sync_channel(1);
        let completion = RcBlock::new(move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The owner waits for this one completion; a disconnected receiver cannot receive another result"
            )]
            let _ = completed.send(());
        });

        // The writer can leave Writing asynchronously, and finishing it then raises an
        // Objective-C exception that aborts the process. This check narrows the race; closing
        // it would need exception catching, which the build omits.
        unsafe {
            ensure!(
                self.native.status() == AVAssetWriterStatus::Writing,
                "The native movie writer stopped before finalization"
            );
            self.native.finishWritingWithCompletionHandler(&completion);
        }
        drop(completion);
        reply
            .recv()
            .context("Movie finalization did not complete")?;
        self.finished = true;

        let status = unsafe { self.native.status() };
        ensure!(
            status == AVAssetWriterStatus::Completed,
            "The native movie couldn't finish"
        );
        self.output.close()
    }
}

impl Drop for Codec {
    fn drop(&mut self) {
        if !self.finished && unsafe { self.native.status() } == AVAssetWriterStatus::Writing {
            unsafe {
                self.native.cancelWriting();
            }
        }
        self.output.retire();
    }
}
