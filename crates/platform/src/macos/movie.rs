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

use anyhow::{Context as _, anyhow, bail};
use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2_av_foundation::{AVAssetImageGenerator, AVAssetImageGeneratorApertureModeEncodedPixels};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use objc2_core_media::CMTime;
use objc2_foundation::{NSError, NSUnderlyingErrorKey};
use speakeasy_show::MoviePrefix;

use crate::{BgraImage, MovieFrame};

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
        Request::start(asset, prefix.size(), sample)?.wait(cancelled)
    })
}

#[derive(Clone, Copy)]
struct Sample {
    ticks: u64,
    timescale: u32,
    at: Duration,
}

impl Sample {
    fn at(prefix: &MoviePrefix, at: Duration) -> anyhow::Result<Self> {
        let samples = prefix.samples();
        let index = samples
            .partition_point(|sample| sample.at <= at)
            .saturating_sub(1);
        let sample = samples.get(index).context("Movie has no retained sample")?;
        Ok(Self {
            ticks: sample.ticks,
            timescale: prefix.timescale(),
            at: sample.at,
        })
    }
}

/// One image request, holding the file-backed asset its generator reads.
///
/// Cancellation and drop still wait for the single completion, so the handler and asset
/// outlive native generation.
struct Request {
    native: Retained<AVAssetImageGenerator>,
    reply: Receiver<anyhow::Result<MovieFrame>>,
    pending: bool,
    _asset: file::Asset,
}

impl Request {
    fn start(asset: file::Asset, size: [u32; 2], sample: Sample) -> anyhow::Result<Self> {
        let ticks = i64::try_from(sample.ticks)?;
        let timescale = i32::try_from(sample.timescale)?;
        let time = unsafe { CMTime::new(ticks, timescale) };
        let (sender, reply) = mpsc::sync_channel(1);
        let handler = RcBlock::new(move |image: *mut CGImage, _: CMTime, error: *mut NSError| {
            // SAFETY: the completion lends both objects until it returns; only copied pixels
            // leave it.
            let (image, error) = unsafe { (image.as_ref(), error.as_ref()) };
            let result = catch_unwind(AssertUnwindSafe(|| {
                autoreleasepool(|_| decoded(image, error, size, sample))
            }))
            .unwrap_or_else(|_| Err(anyhow!("Video frame conversion failed")));
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The owning request drains this completion on cancellation and unwind; a departed receiver needs no second notification"
            )]
            let _ = sender.send(result);
        });

        let native = unsafe {
            let native = AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset.native);
            native.setAppliesPreferredTrackTransform(false);
            native.setApertureMode(Some(AVAssetImageGeneratorApertureModeEncodedPixels));
            native.setMaximumSize(CGSize::new(f64::from(size[0]), f64::from(size[1])));
            // Zero tolerance decodes the validated sample at its exact rational time, not a
            // nearby frame.
            native.setRequestedTimeToleranceBefore(CMTime::new(0, 1));
            native.setRequestedTimeToleranceAfter(CMTime::new(0, 1));
            native.generateCGImageAsynchronouslyForTime_completionHandler(time, &handler);
            native
        };
        Ok(Self {
            native,
            reply,
            pending: true,
            _asset: asset,
        })
    }

    fn wait(mut self, cancelled: &dyn Fn() -> bool) -> anyhow::Result<Option<MovieFrame>> {
        let mut stopping = false;
        let decoded = loop {
            if !stopping && cancelled() {
                stopping = true;
                self.cancel();
            }
            let reply = self.reply.recv_timeout(Duration::from_millis(20));
            self.pending = matches!(reply, Err(RecvTimeoutError::Timeout));
            match reply {
                Ok(decoded) => break decoded,
                Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => bail!("Video decoder ended without a frame"),
            }
        };
        if stopping || cancelled() {
            Ok(None)
        } else {
            decoded.map(Some)
        }
    }

    fn cancel(&self) {
        unsafe { self.native.cancelAllCGImageGeneration() }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        if self.pending {
            self.cancel();
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
    error: Option<&NSError>,
    size: [u32; 2],
    sample: Sample,
) -> anyhow::Result<MovieFrame> {
    let image = image.ok_or_else(|| undecodable(error))?;
    Ok(MovieFrame {
        at: sample.at,
        image: pixels(image, size)?,
    })
}

fn undecodable(error: Option<&NSError>) -> anyhow::Error {
    let Some(error) = error else {
        return anyhow!("macOS couldn't decode this video");
    };
    let code = error.code();
    let key = unsafe { NSUnderlyingErrorKey };
    let underlying = error.userInfo().objectForKey(key);
    match underlying
        .as_deref()
        .and_then(|object| object.downcast_ref::<NSError>())
        .map(NSError::code)
    {
        Some(detail) => anyhow!("macOS couldn't decode this video ({code}, {detail})"),
        None => anyhow!("macOS couldn't decode this video ({code})"),
    }
}

fn pixels(image: &CGImage, size: [u32; 2]) -> anyhow::Result<BgraImage> {
    let [width, height] = size.map(|edge| edge as usize);
    let stride = width.checked_mul(4).context("Video row size overflowed")?;
    let mut pixels = vec![
        0;
        stride
            .checked_mul(height)
            .context("Video canvas overflowed")?
    ];

    {
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .context("Can't create the video color space")?;

        // SAFETY: the context draws into `pixels` and drops at the end of this block, before
        // `pixels` moves.
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
        .context("Can't create the video pixel buffer")?;

        // Bitmap drawing keeps raster row order; a view-style flip would invert the rows.
        CGContext::draw_image(
            Some(&context),
            CGRect::new(
                CGPoint::ZERO,
                CGSize::new(f64::from(size[0]), f64::from(size[1])),
            ),
            Some(image),
        );
    }
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    BgraImage::new(size, pixels).context("Decoded video pixels do not match its canvas")
}
