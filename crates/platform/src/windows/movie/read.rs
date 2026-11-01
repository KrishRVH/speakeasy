//! A synchronous native reader seeks in a validated local file and releases its I/O before return.

use std::{fs::File, time::Duration};

use anyhow::{Context as _, ensure};
use windows::{
    Win32::{
        Media::MediaFoundation::{
            IMFMediaType, IMFSample, IMFSourceReader, IMFSourceReaderEx, MF_E_ATTRIBUTENOTFOUND,
            MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
            MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES,
            MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING,
            MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ALLEFFECTSREMOVED,
            MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
            MF_SOURCE_READERF_ERROR, MF_XVP_DISABLE_FRC, MFCreateAttributes, MFCreateMediaType,
            MFCreateSourceReaderFromByteStream, MFGetStrideForBitmapInfoHeader, MFMediaType_Video,
            MFNominalRange_0_255, MFT_CATEGORY_VIDEO_PROCESSOR, MFVideoFormat_RGB32,
            MFVideoPrimaries_BT709, MFVideoTransFunc_sRGB,
        },
        System::Com::StructuredStorage::PROPVARIANT,
    },
    core::{GUID, Interface as _},
};

use super::{file, foundation::Foundation};
use crate::MovieFrame;

mod pixels;

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned();

pub(crate) fn frame(
    file: File,
    size: [u32; 2],
    at: Duration,
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Option<MovieFrame>> {
    decode(file, size, at, cancelled).map_err(|error| {
        if let Some(native) = error.downcast_ref::<windows::core::Error>() {
            anyhow::anyhow!("Windows could not decode this video ({:?})", native.code())
        } else {
            error
        }
    })
}

fn decode(
    file: File,
    size: [u32; 2],
    at: Duration,
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Option<MovieFrame>> {
    let _foundation = Foundation::start()?;
    let stream = file::read(file)?;
    let reader = reader(&stream)?;
    if cancelled() {
        return Ok(None);
    }
    let ticks = i64::try_from(at.as_nanos() / 100)?;
    // SAFETY: the position variant is owned through this call; zero GUID selects 100 ns time.
    unsafe { reader.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(ticks))? };
    let Some(decoded) = select(&reader, size, ticks, cancelled)? else {
        return Ok(None);
    };
    let bgra = pixels::read(&decoded.sample, decoded.stride, size)?;
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(MovieFrame {
        at: Duration::from_nanos(
            u64::try_from(decoded.at)?
                .checked_mul(100)
                .context("Video timestamp overflowed")?,
        ),
        size,
        bgra,
    }))
}

fn reader(stream: &file::ReadStream) -> anyhow::Result<IMFSourceReader> {
    // SAFETY: all attribute and media type values are documented native encodings. The reader
    // reads only the caller's validated local file through its owned, read-only byte stream.
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&raw mut attributes, 1)?;
        let attributes = attributes.context("Video reader attributes are unavailable")?;
        attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        let reader = MFCreateSourceReaderFromByteStream(&stream.native, &attributes)?;
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0.cast_unsigned(), false)?;
        reader.SetStreamSelection(STREAM, true)?;
        let format = MFCreateMediaType()?;
        format.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        format.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        format.SetUINT32(
            &MF_MT_VIDEO_PRIMARIES,
            u32::try_from(MFVideoPrimaries_BT709.0)?,
        )?;
        format.SetUINT32(
            &MF_MT_TRANSFER_FUNCTION,
            u32::try_from(MFVideoTransFunc_sRGB.0)?,
        )?;
        format.SetUINT32(
            &MF_MT_VIDEO_NOMINAL_RANGE,
            u32::try_from(MFNominalRange_0_255.0)?,
        )?;
        reader.SetCurrentMediaType(STREAM, None, &format)?;
        preserve_timestamps(&reader)?;
        Ok(reader)
    }
}

fn preserve_timestamps(reader: &IMFSourceReader) -> anyhow::Result<()> {
    let extended: IMFSourceReaderEx = reader.cast()?;
    for index in 0..8 {
        let mut category = GUID::zeroed();
        let mut transform = None;
        // SAFETY: this owner has configured the reader but submitted no reads. The returned
        // transform is retained through configuration; its types and sample flow remain owned
        // by the source reader. Disabling frame-rate conversion preserves original sample times.
        unsafe {
            extended.GetTransformForStream(
                STREAM,
                index,
                Some(&raw mut category),
                &raw mut transform,
            )?;
            if category == MFT_CATEGORY_VIDEO_PROCESSOR {
                transform
                    .context("Movie color processor is unavailable")?
                    .GetAttributes()?
                    .SetUINT32(&MF_XVP_DISABLE_FRC, 1)?;
                return Ok(());
            }
        }
    }
    anyhow::bail!("Movie decoder exceeded its processing limit")
}

fn format_stride(format: &IMFMediaType, expected: [u32; 2]) -> anyhow::Result<i32> {
    // SAFETY: the decoder owns this initialized media type; these calls only read attributes.
    unsafe {
        let packed = format.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let size = [
            u32::try_from(packed >> 32)?,
            u32::try_from(packed & u64::from(u32::MAX))?,
        ];
        ensure!(
            size == expected && format.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_RGB32,
            "Decoded video canvas differs from its validated container"
        );
        ensure!(
            format.GetUINT32(&MF_MT_VIDEO_PRIMARIES)? == u32::try_from(MFVideoPrimaries_BT709.0)?
                && format.GetUINT32(&MF_MT_TRANSFER_FUNCTION)?
                    == u32::try_from(MFVideoTransFunc_sRGB.0)?
                && format.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE)?
                    == u32::try_from(MFNominalRange_0_255.0)?,
            "Decoded video color format changed"
        );
        match format.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
            Ok(stride) => Ok(stride.cast_signed()),
            Err(error) if error.code() == MF_E_ATTRIBUTENOTFOUND => Ok(
                MFGetStrideForBitmapInfoHeader(MFVideoFormat_RGB32.data1, size[0])?,
            ),
            Err(error) => Err(error.into()),
        }
    }
}

struct Decoded {
    at: i64,
    sample: IMFSample,
    stride: i32,
}

fn select(
    reader: &IMFSourceReader,
    size: [u32; 2],
    requested: i64,
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Option<Decoded>> {
    // SAFETY: the reader owns its initialized output type; no sample has been selected yet.
    let mut stride = format_stride(&unsafe { reader.GetCurrentMediaType(STREAM)? }, size)?;
    let mut previous: Option<Decoded> = None;
    for _ in 0..10_802 {
        if cancelled() {
            return Ok(None);
        }
        let (mut flags, mut sample) = (0, None);
        // SAFETY: all outputs are live initialized locals; the synchronous reader retains its
        // stream and returns owned samples. No sample's pixels escape their lock.
        unsafe {
            reader.ReadSample(
                STREAM,
                0,
                None,
                Some(&raw mut flags),
                None,
                Some(&raw mut sample),
            )?;
        }
        ensure!(
            flags
                & (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ALLEFFECTSREMOVED.0)
                    .cast_unsigned()
                == 0,
            "Video failed or lost its color processor during decoding"
        );
        if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0.cast_unsigned() != 0 {
            // SAFETY: a successful format-change notification requires querying the new type.
            // Revalidate before admitting this sample, while retaining each earlier sample's stride.
            stride = format_stride(&unsafe { reader.GetCurrentMediaType(STREAM)? }, size)?;
        }
        if let Some(sample) = sample {
            // SAFETY: this sample remains owned throughout the query and subsequent selection.
            let at = unsafe { sample.GetSampleTime()? };
            ensure!(
                (0..=3_600_000_000).contains(&at)
                    && previous.as_ref().is_none_or(|last| at > last.at),
                "Video sample timestamps are invalid"
            );
            let decoded = Decoded { at, sample, stride };
            if at > requested {
                return Ok(Some(previous.unwrap_or(decoded)));
            }
            previous = Some(decoded);
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
            return previous
                .map(Some)
                .context("Video contains no decodable samples");
        }
    }
    anyhow::bail!("Video decoder exceeded the sample limit")
}

#[cfg(test)]
mod tests;
