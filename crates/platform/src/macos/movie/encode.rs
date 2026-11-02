mod buffers;
mod output;
mod settings;
#[cfg(test)]
mod tests;

use std::{
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

use crate::movie::{
    encode::{self, FRAME_LIMIT},
    is_canvas,
};

pub(crate) struct Codec {
    native: Retained<AVAssetWriter>,
    input: Retained<AVAssetWriterInput>,
    output: Retained<output::Output>,
    buffers: buffers::Buffers,
    frames: u32,
    finished: bool,
}

impl encode::Codec for Codec {
    const CLOCK: encode::Clock = encode::Clock::Nanoseconds;

    fn open(path: &Path, size: [u32; 2], limit: u64) -> anyhow::Result<Self> {
        ensure!(is_canvas(size), "Video canvas is invalid");
        let output = output::Output::new(path, limit)?;
        let buffers = buffers::Buffers::new(size)?;
        let settings = settings::video(size)?;

        let (native, input) = unsafe {
            let kind = UTType::typeWithIdentifier(AVFileTypeMPEG4.context("MP4 is unavailable")?)
                .context("MP4 type is unavailable")?;
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
                "The native movie writer could not start"
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
    ) -> anyhow::Result<bool> {
        ensure!(
            index == self.frames && index < FRAME_LIMIT,
            "The movie reached its recording limit"
        );
        let began = Instant::now();
        loop {
            if cancelled() {
                return Ok(false);
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
        let sample = self.buffers.sample(bgra, self.frames)?;
        if cancelled() {
            return Ok(false);
        }

        let appended = unsafe { self.input.appendSampleBuffer(&sample) };
        ensure!(appended, "The native movie writer refused a frame");
        self.frames = self
            .frames
            .checked_add(1)
            .context("Movie sample count overflowed")?;
        Ok(true)
    }

    fn checkpoint(&mut self, _: Duration) -> anyhow::Result<()> {
        self.output.check()
    }

    fn finish(mut self) -> anyhow::Result<()> {
        ensure!(self.frames > 0, "The movie contains no frames");
        let (completed, reply) = mpsc::sync_channel(1);
        let completion = RcBlock::new(move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The owner waits for this one completion; a disconnected receiver cannot receive another result"
            )]
            let _ = completed.send(());
        });

        unsafe {
            self.native.finishWritingWithCompletionHandler(&completion);
        }
        reply
            .recv()
            .context("Movie finalization did not complete")?;
        self.finished = true;

        let status = unsafe { self.native.status() };
        ensure!(
            status == AVAssetWriterStatus::Completed,
            "The native movie could not finish"
        );
        self.output.close()
    }
}

impl Drop for Codec {
    fn drop(&mut self) {
        if !self.finished {
            unsafe {
                self.native.cancelWriting();
            }
        }
        self.output.retire();
    }
}
