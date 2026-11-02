use std::{
    ptr::{self, NonNull},
    slice,
};

use anyhow::{Context as _, ensure};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
use objc2_core_media::{
    CMSampleBuffer, CMSampleTimingInfo, CMTime, CMVideoFormatDescriptionCreateForImageBuffer,
    kCMTimeInvalid,
};
use objc2_core_video::{
    CVAttachmentMode, CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetDataSize, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferPool, CVPixelBufferUnlockBaseAddress, kCVImageBufferColorPrimaries_ITU_R_709_2,
    kCVImageBufferColorPrimariesKey, kCVImageBufferTransferFunction_sRGB,
    kCVImageBufferTransferFunctionKey, kCVImageBufferYCbCrMatrix_ITU_R_709_2,
    kCVImageBufferYCbCrMatrixKey, kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelBufferPoolAllocationThresholdKey, kCVPixelBufferWidthKey, kCVPixelFormatType_32BGRA,
    kCVReturnSuccess,
};

const BUDGET: usize = 64 << 20;

pub(super) struct Buffers {
    pool: CFRetained<CVPixelBufferPool>,
    threshold: CFRetained<CFDictionary<CFString, CFNumber>>,
    bytes: usize,
    size: [u32; 2],
}

impl Buffers {
    pub(super) fn new(size: [u32; 2]) -> anyhow::Result<Self> {
        let width = CFNumber::new_i64(i64::from(size[0]));
        let height = CFNumber::new_i64(i64::from(size[1]));
        let format = CFNumber::new_i64(i64::from(kCVPixelFormatType_32BGRA));
        let mut pool = ptr::null_mut();

        let pool = unsafe {
            let attributes = CFDictionary::from_slices(
                &[
                    kCVPixelBufferWidthKey,
                    kCVPixelBufferHeightKey,
                    kCVPixelBufferPixelFormatTypeKey,
                ],
                &[&*width, &*height, &*format],
            );
            ensure!(
                CVPixelBufferPool::create(
                    None,
                    None,
                    Some(attributes.as_opaque()),
                    NonNull::from(&mut pool)
                ) == kCVReturnSuccess,
                "Cannot create native video buffers"
            );
            CFRetained::from_raw(NonNull::new(pool).context("Native video pool is missing")?)
        };
        let mut buffer = ptr::null_mut();

        let buffer = unsafe {
            ensure!(
                CVPixelBufferPool::create_pixel_buffer(None, &pool, NonNull::from(&mut buffer))
                    == kCVReturnSuccess,
                "Cannot size native video buffers"
            );
            CFRetained::from_raw(NonNull::new(buffer).context("Native video buffer is missing")?)
        };
        let bytes = CVPixelBufferGetDataSize(&buffer);
        ensure!(
            (1..=BUDGET).contains(&bytes),
            "Native video buffer exceeds its budget"
        );
        let count = CFNumber::new_i64(i64::try_from(
            BUDGET
                .checked_div(bytes)
                .context("Native video buffer is empty")?,
        )?);

        let threshold = unsafe {
            CFDictionary::from_slices(&[kCVPixelBufferPoolAllocationThresholdKey], &[&*count])
        };
        Ok(Self {
            pool,
            threshold,
            bytes,
            size,
        })
    }

    pub(super) fn sample(
        &self,
        bgra: &[u8],
        index: u32,
    ) -> anyhow::Result<CFRetained<CMSampleBuffer>> {
        let row = usize::try_from(self.size[0])?
            .checked_mul(4)
            .context("Video row overflows")?;
        let height = usize::try_from(self.size[1])?;
        ensure!(
            row.checked_mul(height) == Some(bgra.len()),
            "Video canvas bytes changed"
        );
        let mut buffer = ptr::null_mut();

        let buffer = unsafe {
            ensure!(
                CVPixelBufferPool::create_pixel_buffer_with_aux_attributes(
                    None,
                    &self.pool,
                    Some(self.threshold.as_opaque()),
                    NonNull::from(&mut buffer)
                ) == kCVReturnSuccess,
                "Native video buffers reached their budget"
            );
            CFRetained::from_raw(NonNull::new(buffer).context("Native video buffer is missing")?)
        };
        ensure!(
            CVPixelBufferGetDataSize(&buffer) == self.bytes,
            "Native video buffer size changed"
        );
        let stride = CVPixelBufferGetBytesPerRow(&buffer);
        ensure!(stride >= row, "Native video stride is too short");
        let length = stride
            .checked_mul(height)
            .filter(|length| *length <= self.bytes)
            .context("Native video rows exceed their buffer")?;
        {
            let mut locked = Locked::new(&buffer)?;
            let pixels = locked.bytes(length)?;
            pixels.fill(0);
            for (to, from) in pixels.chunks_exact_mut(stride).zip(bgra.chunks_exact(row)) {
                to.get_mut(..row)
                    .context("Native video row is incomplete")?
                    .copy_from_slice(from);
            }
        }
        let mut format = ptr::null();
        let mut sample = ptr::null_mut();

        unsafe {
            for (key, value) in [
                (
                    kCVImageBufferColorPrimariesKey,
                    kCVImageBufferColorPrimaries_ITU_R_709_2,
                ),
                (
                    kCVImageBufferTransferFunctionKey,
                    kCVImageBufferTransferFunction_sRGB,
                ),
                (
                    kCVImageBufferYCbCrMatrixKey,
                    kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                ),
            ] {
                buffer.set_attachment(key, value.as_ref(), CVAttachmentMode::ShouldPropagate);
            }
            ensure!(
                CMVideoFormatDescriptionCreateForImageBuffer(
                    None,
                    &buffer,
                    NonNull::from(&mut format)
                ) == 0,
                "Cannot describe native video pixels"
            );
            let format = CFRetained::from_raw(
                NonNull::new(format.cast_mut()).context("Native video format is missing")?,
            );
            let mut timing = CMSampleTimingInfo {
                duration: CMTime::new(1, 30),
                presentationTimeStamp: CMTime::new(i64::from(index), 30),
                decodeTimeStamp: kCMTimeInvalid,
            };
            ensure!(
                CMSampleBuffer::create_ready_with_image_buffer(
                    None,
                    &buffer,
                    &format,
                    NonNull::from(&mut timing),
                    NonNull::from(&mut sample)
                ) == 0,
                "Cannot create a native video sample"
            );
            Ok(CFRetained::from_raw(
                NonNull::new(sample).context("Native video sample is missing")?,
            ))
        }
    }
}

struct Locked<'a>(&'a CVPixelBuffer);

impl<'a> Locked<'a> {
    fn new(buffer: &'a CVPixelBuffer) -> anyhow::Result<Self> {
        let status = unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags(0)) };
        ensure!(
            status == kCVReturnSuccess,
            "Cannot write native video pixels"
        );
        Ok(Self(buffer))
    }

    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "An exclusive borrow prevents two mutable slices of the native pixel storage from coexisting"
    )]
    fn bytes(&mut self, length: usize) -> anyhow::Result<&mut [u8]> {
        let base = NonNull::new(CVPixelBufferGetBaseAddress(self.0).cast::<u8>())
            .context("Native video pixels are missing")?;

        Ok(unsafe { slice::from_raw_parts_mut(base.as_ptr(), length) })
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        unsafe {
            CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags(0));
        }
    }
}
