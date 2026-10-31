//! Native pixel locks become bounded, top-down opaque BGRA rows before leaving the decoder.

use anyhow::{Context as _, ensure};
use windows::{
    Win32::Media::MediaFoundation::{
        IMF2DBuffer2, IMFMediaBuffer, IMFSample, MF2DBuffer_LockFlags_Read,
    },
    core::Interface as _,
};

enum Lock<'a> {
    Flat(&'a IMFMediaBuffer),
    Rows(IMF2DBuffer2),
}

impl Drop for Lock<'_> {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Every successful native lock must unlock on errors as well as success before its buffer is released"
        )]
        // SAFETY: this guard represents exactly one successful lock on the still-owned buffer.
        let _ = unsafe {
            match self {
                Self::Flat(buffer) => buffer.Unlock(),
                Self::Rows(buffer) => buffer.Unlock2D(),
            }
        };
    }
}

pub(super) fn read(sample: &IMFSample, stride: i32, size: [u32; 2]) -> anyhow::Result<Vec<u8>> {
    // SAFETY: the sample owns its buffer. Both lock paths retain the buffer and unlock it before
    // return. Native buffer bounds are checked before creating slices or reading rows.
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let (mut start, mut scanline) = (std::ptr::null_mut(), std::ptr::null_mut());
        let mut length = 0;
        let mut pitch = stride;
        let (lock, offset) = if let Ok(rows) = buffer.cast::<IMF2DBuffer2>() {
            rows.Lock2DSize(
                MF2DBuffer_LockFlags_Read,
                &raw mut scanline,
                &raw mut pitch,
                &raw mut start,
                &raw mut length,
            )?;
            let lock = Lock::Rows(rows);
            let offset = scanline
                .addr()
                .checked_sub(start.addr())
                .context("Video scanline lies outside its buffer")?;
            (lock, offset)
        } else {
            buffer.Lock(&raw mut start, None, Some(&raw mut length))?;
            let lock = Lock::Flat(&buffer);
            let offset = if pitch < 0 {
                usize::try_from(size[1].saturating_sub(1))?
                    .checked_mul(usize::try_from(pitch.unsigned_abs())?)
                    .context("Video row offset overflowed")?
            } else {
                0
            };
            (lock, offset)
        };
        ensure!(
            !start.is_null() && length <= 128 << 20,
            "Decoded video buffer is invalid"
        );
        let bytes = std::slice::from_raw_parts(start, usize::try_from(length)?);
        let pixels = copy(bytes, offset, pitch, size)?;
        drop(lock);
        Ok(pixels)
    }
}

fn copy(
    bytes: &[u8],
    top: usize,
    pitch: i32,
    [width, height]: [u32; 2],
) -> anyhow::Result<Vec<u8>> {
    let row = usize::try_from(width)?
        .checked_mul(4)
        .context("Video row is too large")?;
    ensure!(
        usize::try_from(pitch.unsigned_abs())? >= row,
        "Video rows overlap"
    );
    let mut pixels = Vec::with_capacity(
        row.checked_mul(usize::try_from(height)?)
            .context("Video canvas is too large")?,
    );
    for y in 0..height {
        let offset = i64::from(y)
            .checked_mul(i64::from(pitch))
            .context("Video row offset overflowed")?;
        let begin = top
            .checked_add_signed(isize::try_from(offset)?)
            .context("Video row precedes its buffer")?;
        let end = begin
            .checked_add(row)
            .context("Video row offset overflowed")?;
        pixels.extend_from_slice(
            bytes
                .get(begin..end)
                .context("Video row exceeds its buffer")?,
        );
    }
    for [_, _, _, alpha] in pixels.as_chunks_mut::<4>().0 {
        *alpha = 255;
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_respect_signed_pitch_padding_and_opaque_alpha() -> anyhow::Result<()> {
        let bytes = [1, 2, 3, 0, 0, 0, 0, 0, 4, 5, 6, 0];
        assert_eq!(copy(&bytes, 0, 8, [1, 2])?, [1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(copy(&bytes, 8, -8, [1, 2])?, [4, 5, 6, 255, 1, 2, 3, 255]);
        assert!(copy(&bytes, 0, -8, [1, 2]).is_err());
        assert!(copy(&bytes, 8, 8, [1, 2]).is_err());
        assert!(copy(&bytes, 0, 3, [1, 2]).is_err());
        Ok(())
    }
}
