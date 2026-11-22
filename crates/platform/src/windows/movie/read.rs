use std::{fs::File, time::Duration};

use anyhow::{Context as _, ensure};
use windows::{
    Win32::{
        Media::MediaFoundation::{
            IMFMediaType, IMFSample, IMFSourceReader, IMFSourceReaderEx, MF_E_ATTRIBUTENOTFOUND,
            MF_MT_DEFAULT_STRIDE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION,
            MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_SOURCE_READER_ALL_STREAMS,
            MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
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

const COLOR_FORMAT: [(GUID, u32); 3] = [
    (
        MF_MT_VIDEO_PRIMARIES,
        MFVideoPrimaries_BT709.0.cast_unsigned(),
    ),
    (
        MF_MT_TRANSFER_FUNCTION,
        MFVideoTransFunc_sRGB.0.cast_unsigned(),
    ),
    (
        MF_MT_VIDEO_NOMINAL_RANGE,
        MFNominalRange_0_255.0.cast_unsigned(),
    ),
];

pub(crate) fn frame(
    file: File,
    bytes: u64,
    size: [u32; 2],
    at: Duration,
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Option<MovieFrame>> {
    decode(file, bytes, size, at, cancelled).map_err(|error| {
        if let Some(native) = error.downcast_ref::<windows::core::Error>() {
            anyhow::anyhow!("Windows could not decode this video ({})", native.code())
        } else {
            error
        }
    })
}

fn decode(
    file: File,
    bytes: u64,
    size: [u32; 2],
    at: Duration,
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Option<MovieFrame>> {
    let _foundation = Foundation::start()?;
    let stream = file::read(file, bytes)?;
    let reader = reader(&stream)?;
    if cancelled() {
        return Ok(None);
    }
    let ticks = i64::try_from(at.as_nanos() / 100)?;

    unsafe { reader.SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(ticks))? };
    let Some(decoded) = select(&reader, size, ticks, cancelled)? else {
        return Ok(None);
    };
    let image = pixels::read(&decoded.sample, decoded.stride, size)?;
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(MovieFrame {
        at: Duration::from_nanos(
            u64::try_from(decoded.at)?
                .checked_mul(100)
                .context("Video timestamp overflowed")?,
        ),
        image,
    }))
}

fn reader(stream: &file::ReadStream) -> anyhow::Result<IMFSourceReader> {
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
        for (attribute, value) in &COLOR_FORMAT {
            format.SetUINT32(attribute, *value)?;
        }
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

fn format_stride(format: &IMFMediaType, width: u32) -> anyhow::Result<i32> {
    unsafe {
        match format.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
            Ok(stride) => Ok(stride.cast_signed()),
            Err(error) if error.code() == MF_E_ATTRIBUTENOTFOUND => Ok(
                MFGetStrideForBitmapInfoHeader(MFVideoFormat_RGB32.data1, width)?,
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
    let mut stride = format_stride(&unsafe { reader.GetCurrentMediaType(STREAM)? }, size[0])?;
    let mut previous: Option<Decoded> = None;
    loop {
        if cancelled() {
            return Ok(None);
        }
        let (mut flags, mut sample) = (0, None);

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
            flags & MF_SOURCE_READERF_ERROR.0.cast_unsigned() == 0,
            "Video failed during decoding"
        );
        if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0.cast_unsigned() != 0 {
            stride = format_stride(&unsafe { reader.GetCurrentMediaType(STREAM)? }, size[0])?;
        }
        if let Some(sample) = sample {
            let at = unsafe { sample.GetSampleTime()? };
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
}

#[cfg(test)]
mod tests;
