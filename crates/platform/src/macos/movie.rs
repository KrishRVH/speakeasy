//! Exact native frame requests retain their verified input through completion and teardown.

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

use crate::MovieFrame;

pub(crate) fn frame(
    input: File,
    prefix: &speakeasy_show::MoviePrefix,
    sample: (u64, u32),
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Option<MovieFrame>> {
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
                // SAFETY: only this owner controls generation. Cancellation still waits for the
                // one promised completion before the generator and input lease are released.
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

struct Request {
    native: Retained<AVAssetImageGenerator>,
    reply: Receiver<anyhow::Result<MovieFrame>>,
    pending: bool,
}

impl Request {
    fn start(asset: &AVAsset, size: [u32; 2], sample: (u64, u32)) -> anyhow::Result<Self> {
        // SAFETY: validation bounds the nonnegative ticks and strictly positive timescale.
        let time = unsafe { CMTime::new(i64::try_from(sample.0)?, i32::try_from(sample.1)?) };
        let (sender, reply) = mpsc::sync_channel(1);
        let handler = RcBlock::new(
            move |image: *mut CGImage, at: CMTime, error: *mut NSError| {
                // SAFETY: completion lends both optional objects until it returns. Only copied
                // pixels and checked timing leave this callback.
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
        // SAFETY: the callback captures only Send Rust values. The asset is retained by the
        // generator, and the caller retains its verified-file lease until this request drains.
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

fn decoded(
    image: Option<&CGImage>,
    at: CMTime,
    error: Option<&NSError>,
    size: [u32; 2],
    sample: (u64, u32),
) -> anyhow::Result<MovieFrame> {
    let image = image.ok_or_else(|| {
        let code = error.map(NSError::code);
        // SAFETY: Foundation's immutable dictionary key is available on every supported macOS.
        let key = unsafe { objc2_foundation::NSUnderlyingErrorKey };
        let underlying = error.and_then(|error| error.userInfo().objectForKey(key));
        let detail = underlying
            .as_ref()
            .and_then(|object| object.downcast_ref::<NSError>())
            .map(NSError::code);
        anyhow::anyhow!("macOS could not decode this video ({code:?}, {detail:?})")
    })?;
    Ok(MovieFrame {
        at: timestamp(at, sample)?,
        size,
        bgra: pixels(image, size)?,
    })
}

impl Drop for Request {
    fn drop(&mut self) {
        if self.pending {
            // SAFETY: the request owns this generator and must wait even while its caller unwinds.
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

fn timestamp(time: CMTime, expected: (u64, u32)) -> anyhow::Result<Duration> {
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
        ticks.checked_mul(u128::from(expected.1)) == u128::from(expected.0).checked_mul(scale),
        "Video decoder returned a different sample"
    );
    Ok(Duration::from_nanos(u64::try_from(
        ticks
            .checked_mul(1_000_000_000)
            .and_then(|nanos| nanos.checked_div(scale))
            .context("Video timestamp overflowed")?,
    )?))
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
    // SAFETY: the permanent sRGB name selects the same color space as saved Show stills. The
    // context borrows the bounded pixel allocation and is destroyed before that allocation moves.
    {
        // SAFETY: the color-space name is an immutable process-wide native constant.
        let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
            .context("Cannot create the video color space")?;
        // SAFETY: the context borrows this live bounded allocation and is released before it moves.
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
        // Bitmap drawing preserves raster row order; a view-coordinate flip would invert it.
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
