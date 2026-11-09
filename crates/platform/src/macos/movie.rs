mod file;

pub(crate) mod encode;
#[cfg(test)]
mod tests;

use std::{
    fs::File,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    time::Duration,
};

use anyhow::{Context as _, ensure};
use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2_av_foundation::{
    AVAsset, AVAssetImageGenerator, AVAssetImageGeneratorApertureModeEncodedPixels,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use objc2_core_media::{CMTime, CMTimeFlags};
use objc2_foundation::NSError;
use speakeasy_show::MoviePrefix;

use crate::MovieFrame;

pub(crate) fn frame(
    input: File,
    prefix: &MoviePrefix,
    at: Duration,
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Option<MovieFrame>> {
    let sample = Sample::at(prefix, at)?;
    autoreleasepool(|_| {
        let asset = file::Asset::new(input, prefix)?;
        if cancelled() {
            return Ok(None);
        }
        let mut request = Request::start(&asset.native, prefix.size, sample)?;
        let mut stopping = false;
        loop {
            if !stopping && cancelled() {
                stopping = true;

                unsafe {
                    request.native.cancelAllCGImageGeneration();
                }
            }
            match request.reply.recv_timeout(Duration::from_millis(20)) {
                Ok(result) => {
                    request.pending = false;
                    return if stopping || cancelled() {
                        Ok(None)
                    } else {
                        result.map(Some)
                    };
                },
                Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => {
                    request.pending = false;
                    anyhow::bail!("Video decoder ended without a frame");
                },
            }
        }
    })
}

#[derive(Clone, Copy)]
struct Sample {
    ticks: u64,
    timescale: u32,
}

impl Sample {
    fn at(prefix: &MoviePrefix, at: Duration) -> anyhow::Result<Self> {
        let samples = prefix.samples();
        let index = samples
            .partition_point(|sample| sample.at <= at)
            .saturating_sub(1);
        let sample = samples.get(index).context("Movie has no retained sample")?;
        ensure!(
            (1..=1_000_000_000).contains(&prefix.timescale)
                && sample.ticks
                    <= u64::from(prefix.timescale)
                        .saturating_mul(MoviePrefix::DURATION_LIMIT.as_secs()),
            "Movie sample timestamp is out of range"
        );
        Ok(Self {
            ticks: sample.ticks,
            timescale: prefix.timescale,
        })
    }

    fn timestamp(self, time: CMTime) -> anyhow::Result<Duration> {
        ensure!(
            time.flags.contains(CMTimeFlags::Valid)
                && !time.flags.intersects(CMTimeFlags::ImpliedValueFlagsMask)
                && time.epoch == 0
                && time.value >= 0
                && time.timescale > 0,
            "Video sample timestamp is invalid"
        );
        let ticks = u128::try_from(time.value)?;
        let scale = u128::try_from(time.timescale)?;
        ensure!(
            ticks.checked_mul(u128::from(self.timescale))
                == u128::from(self.ticks).checked_mul(scale),
            "Video decoder returned a different sample"
        );
        Ok(Duration::from_nanos(u64::try_from(
            ticks
                .checked_mul(1_000_000_000)
                .and_then(|nanos| nanos.checked_div(scale))
                .context("Video timestamp overflowed")?,
        )?))
    }
}

struct Request {
    native: Retained<AVAssetImageGenerator>,
    reply: Receiver<anyhow::Result<MovieFrame>>,
    pending: bool,
}

impl Request {
    fn start(asset: &AVAsset, size: [u32; 2], sample: Sample) -> anyhow::Result<Self> {
        let ticks = i64::try_from(sample.ticks)?;
        let timescale = i32::try_from(sample.timescale)?;
        let time = unsafe { CMTime::new(ticks, timescale) };
        let (sender, reply) = mpsc::sync_channel(1);
        let handler = RcBlock::new(
            move |image: *mut CGImage, at: CMTime, error: *mut NSError| {
                let (image, error) = unsafe { (image.as_ref(), error.as_ref()) };
                let result = catch_unwind(AssertUnwindSafe(|| {
                    autoreleasepool(|_| decoded(image, at, error, size, sample))
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Video frame conversion failed")));
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "The owning request drains this completion on cancellation and unwind; a departed receiver needs no second notification"
                )]
                let _ = sender.send(result);
            },
        );

        let native = unsafe {
            let native = AVAssetImageGenerator::assetImageGeneratorWithAsset(asset);
            native.setAppliesPreferredTrackTransform(false);
            native.setApertureMode(Some(AVAssetImageGeneratorApertureModeEncodedPixels));
            native.setMaximumSize(CGSize::new(f64::from(size[0]), f64::from(size[1])));
            native.setRequestedTimeToleranceBefore(CMTime::new(0, 1));
            native.setRequestedTimeToleranceAfter(CMTime::new(0, 1));
            native.generateCGImageAsynchronouslyForTime_completionHandler(time, &handler);
            native
        };
        Ok(Self {
            native,
            reply,
            pending: true,
        })
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        if self.pending {
            unsafe {
                self.native.cancelAllCGImageGeneration();
            }
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Unwinding retires native work by waiting for its completion; no image result can be delivered to the departed caller"
            )]
            let _ = self.reply.recv();
        }
    }
}

fn decoded(
    image: Option<&CGImage>,
    at: CMTime,
    error: Option<&NSError>,
    size: [u32; 2],
    sample: Sample,
) -> anyhow::Result<MovieFrame> {
    let image = image.ok_or_else(|| {
        let code = error.map(NSError::code);

        let key = unsafe { objc2_foundation::NSUnderlyingErrorKey };
        let underlying = error.and_then(|error| error.userInfo().objectForKey(key));
        let detail = underlying
            .as_ref()
            .and_then(|object| object.downcast_ref::<NSError>())
            .map(NSError::code);
        anyhow::anyhow!("macOS could not decode this video ({code:?}, {detail:?})")
    })?;
    Ok(MovieFrame {
        at: sample.timestamp(at)?,
        size,
        bgra: pixels(image, size)?,
    })
}

fn pixels(image: &CGImage, size: [u32; 2]) -> anyhow::Result<Vec<u8>> {
    let [width, height] = size.map(|edge| edge as usize);
    ensure!(
        CGImage::width(Some(image)) == width && CGImage::height(Some(image)) == height,
        "Decoded video canvas differs from its validated container"
    );
    let stride = width.checked_mul(4).context("Video row size overflowed")?;
    let mut pixels = vec![
        0;
        stride
            .checked_mul(height)
            .context("Video canvas overflowed")?
    ];

    {
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .context("Cannot create the video color space")?;

        let context = unsafe {
            CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width,
                height,
                8,
                stride,
                Some(&space),
                CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::PremultipliedFirst.0,
            )
        }
        .context("Cannot create the video pixel buffer")?;

        CGContext::draw_image(
            Some(&context),
            CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(f64::from(size[0]), f64::from(size[1])),
            ),
            Some(image),
        );
    }
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    Ok(pixels)
}
