//! Native objects stay on the capture thread. Field order releases the writer, shuts down its
//! sink, closes its stream, and only then leaves Media Foundation and the COM apartment.

use std::{path::Path, time::Duration};

use anyhow::{Context as _, ensure};
use windows::{
    Win32::Media::MediaFoundation::{
        IMFAttributes, IMFMediaBuffer, IMFMediaSink, IMFMediaType, IMFSample, IMFSinkWriter,
        MF_LOW_LATENCY, MF_MPEG4SINK_MAX_CODED_SEQUENCES_PER_FRAGMENT,
        MF_MPEG4SINK_MIN_FRAGMENT_DURATION, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE,
        MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
        MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
        MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES,
        MF_MT_YUV_MATRIX, MF_READWRITE_DISABLE_CONVERTERS, MF_SINK_WRITER_DISABLE_THROTTLING,
        MF_SINK_WRITER_STATISTICS, MFCreateAttributes, MFCreateFMPEG4MediaSink, MFCreateMediaType,
        MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromMediaSink, MFMediaType_Video,
        MFNominalRange_0_255, MFNominalRange_16_235, MFVideoFormat_H264, MFVideoFormat_NV12,
        MFVideoFormat_RGB32, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709,
        MFVideoTransFunc_sRGB, MFVideoTransferMatrix_BT709,
    },
    core::{GUID, Interface as _},
};

use super::{convert::Converter, file, foundation::Foundation};
use crate::movie::encode::{self, Clock};

const TICKS: u64 = 10_000_000;
const QUEUED_BYTES: u32 = 64 << 20;

pub(crate) struct Codec {
    writer: IMFSinkWriter,
    converter: Converter,
    _sink: Sink,
    stream: file::Stream,
    synced: Duration,
    _foundation: Foundation,
}

struct Sink(IMFMediaSink);

impl Drop for Sink {
    fn drop(&mut self) {
        // SAFETY: this guard owns a live sink on the initializing thread; its writer is gone.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Shutdown still runs after failed finalization; recovery inspects the surviving file instead of assuming it is complete"
        )]
        // SAFETY: this guard owns the live sink after all writer submissions end.
        let _ = unsafe { self.0.Shutdown() };
    }
}

impl encode::Codec for Codec {
    const CLOCK: Clock = Clock::HundredNanoseconds;

    fn open(path: &Path, size: [u32; 2], limit: u64) -> anyhow::Result<Self> {
        let foundation = Foundation::start()?;
        let stream = file::create(path, limit)?;
        let output = media(&MFVideoFormat_H264, size)?;
        // SAFETY: the attributes and media types are owned, initialized COM objects. Values are
        // documented native encodings; none borrows Rust memory beyond these calls.
        let (sink, writer, converter) = unsafe {
            output.SetUINT32(&MF_MT_AVG_BITRATE, 8_000_000)?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, 100)?;
            output.SetUINT32(
                &MF_MT_VIDEO_NOMINAL_RANGE,
                u32::try_from(MFNominalRange_16_235.0)?,
            )?;
            output.SetUINT32(
                &MF_MT_YUV_MATRIX,
                u32::try_from(MFVideoTransferMatrix_BT709.0)?,
            )?;
            let sink = Sink(MFCreateFMPEG4MediaSink(&stream.native, &output, None)?);
            let fragments: IMFAttributes = sink.0.cast()?;
            fragments.SetUINT64(&MF_MPEG4SINK_MIN_FRAGMENT_DURATION, TICKS)?;
            fragments.SetUINT32(&MF_MPEG4SINK_MAX_CODED_SEQUENCES_PER_FRAGMENT, 1)?;
            let mut attributes = None;
            MFCreateAttributes(&raw mut attributes, 3)?;
            let attributes = attributes.context("Video writer attributes are unavailable")?;
            // CPU-backed NV12 frames use the system software path. Hardware transform discovery
            // delays the first frame; a GPU encoder needs a D3D surface input and device manager.
            attributes.SetUINT32(&MF_LOW_LATENCY, 1)?;
            attributes.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1)?;
            attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            let writer = MFCreateSinkWriterFromMediaSink(&sink.0, &attributes)?;
            let input = media(&MFVideoFormat_RGB32, size)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, size[0].saturating_mul(4))?;
            input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            input.SetUINT32(
                &MF_MT_VIDEO_NOMINAL_RANGE,
                u32::try_from(MFNominalRange_0_255.0)?,
            )?;
            let format = media(&MFVideoFormat_NV12, size)?;
            format.SetUINT32(
                &MF_MT_YUV_MATRIX,
                u32::try_from(MFVideoTransferMatrix_BT709.0)?,
            )?;
            format.SetUINT32(
                &MF_MT_VIDEO_NOMINAL_RANGE,
                u32::try_from(MFNominalRange_16_235.0)?,
            )?;
            let (converter, format) = Converter::new(&input, &format)?;
            writer.SetInputMediaType(0, &format, None)?;
            writer.BeginWriting()?;
            (sink, writer, converter)
        };
        Ok(Self {
            writer,
            converter,
            _sink: sink,
            stream,
            synced: Duration::ZERO,
            _foundation: foundation,
        })
    }

    fn write(
        &mut self,
        pixels: &[u8],
        index: u32,
        _cancelled: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<bool> {
        let at = Self::CLOCK.time(index);
        let through = Self::CLOCK.time(index.saturating_add(1));
        submit(
            &self.writer,
            &self.converter,
            pixels,
            at,
            through.saturating_sub(at),
        )?;
        Ok(true)
    }

    fn checkpoint(&mut self, at: Duration) -> anyhow::Result<()> {
        if at.saturating_sub(self.synced) >= Duration::from_secs(1) {
            // SAFETY: the byte stream stays open until finalization; Flush syncs its file.
            unsafe { self.stream.native.Flush()? };
            self.synced = at;
        }
        Ok(())
    }

    fn finish(self) -> anyhow::Result<()> {
        self.converter.finish()?;
        // SAFETY: all input submissions ended. Finalize closes the stream; no later Flush may
        // touch it. Field teardown still shuts the sink down when Finalize fails.
        unsafe { self.writer.Finalize()? };
        self.stream.sync.sync_data()?;
        Ok(())
    }
}

fn submit(
    writer: &IMFSinkWriter,
    converter: &Converter,
    pixels: &[u8],
    at: Duration,
    duration: Duration,
) -> anyhow::Result<()> {
    let mut statistics = MF_SINK_WRITER_STATISTICS {
        cb: u32::try_from(size_of::<MF_SINK_WRITER_STATISTICS>())?,
        ..MF_SINK_WRITER_STATISTICS::default()
    };
    // SAFETY: the output is initialized, sized, and writable for this synchronous query.
    unsafe { writer.GetStatistics(0, &raw mut statistics)? };
    let bytes = u32::try_from(pixels.len())?;
    ensure!(
        statistics.dwByteCountQueued.saturating_add(bytes) <= QUEUED_BYTES,
        "Video encoding cannot keep up; retained frames remain available"
    );
    let sample = converter.frame(&sample(pixels, at, duration)?)?;
    // SAFETY: the sample owns its buffer. WriteSample retains it for deferred encoding; the actual
    // stream enforces its byte cap even during later writes. Held tail samples use this same gate.
    unsafe { writer.WriteSample(0, &sample)? };
    Ok(())
}

fn media(kind: &GUID, [width, height]: [u32; 2]) -> anyhow::Result<IMFMediaType> {
    // SAFETY: every attribute is set on this new owned media type, using bounded canvas values.
    unsafe {
        let media = MFCreateMediaType()?;
        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media.SetGUID(&MF_MT_SUBTYPE, kind)?;
        media.SetUINT32(
            &MF_MT_VIDEO_PRIMARIES,
            u32::try_from(MFVideoPrimaries_BT709.0)?,
        )?;
        media.SetUINT32(
            &MF_MT_TRANSFER_FUNCTION,
            u32::try_from(MFVideoTransFunc_sRGB.0)?,
        )?;
        media.SetUINT64(
            &MF_MT_FRAME_SIZE,
            (u64::from(width) << 32) | u64::from(height),
        )?;
        media.SetUINT64(&MF_MT_FRAME_RATE, (30_u64 << 32) | 1)?;
        media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1_u64 << 32) | 1)?;
        media.SetUINT32(
            &MF_MT_INTERLACE_MODE,
            MFVideoInterlace_Progressive.0.cast_unsigned(),
        )?;
        Ok(media)
    }
}

struct Locked<'a>(&'a IMFMediaBuffer);

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: this guard exists only after Lock succeeded and drops before its buffer.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Unlock is mandatory on every partial sample construction path; an unsuccessful sample is never submitted"
        )]
        // SAFETY: this guard owns exactly one successful Lock on the live buffer.
        let _ = unsafe { self.0.Unlock() };
    }
}

fn sample(pixels: &[u8], at: Duration, duration: Duration) -> anyhow::Result<IMFSample> {
    let bytes = u32::try_from(pixels.len())?;
    // SAFETY: the allocation is bounded by Canvas. The lock guard releases the borrowed native
    // buffer on every path, and only its reported writable length is copied.
    unsafe {
        let buffer = MFCreateMemoryBuffer(bytes)?;
        {
            let mut pointer = std::ptr::null_mut();
            let mut capacity = 0;
            buffer.Lock(&raw mut pointer, Some(&raw mut capacity), None)?;
            let _locked = Locked(&buffer);
            ensure!(
                !pointer.is_null() && capacity >= bytes,
                "The movie encoder returned a short buffer"
            );
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), pointer, pixels.len());
        }
        buffer.SetCurrentLength(bytes)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(i64::try_from(at.as_nanos() / 100)?)?;
        sample.SetSampleDuration(i64::try_from(duration.as_nanos() / 100)?)?;
        Ok(sample)
    }
}

#[cfg(test)]
pub(super) fn sample_times(path: &Path) -> anyhow::Result<Vec<(i64, i64)>> {
    use windows::{
        Win32::Media::MediaFoundation::{
            MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM,
            MFCreateSourceReaderFromURL,
        },
        core::HSTRING,
    };
    let _foundation = Foundation::start()?;
    // SAFETY: this opt-in test reads only its own newly encoded synthetic file. The reader is
    // released before the Foundation guard; every output is owned and valid for the native call.
    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None)?;
        let mut times = Vec::new();
        loop {
            ensure!(
                times.len() <= 9_002,
                "Synthetic movie exceeds its frame bound"
            );
            let mut flags = 0;
            let mut sample = None;
            reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0.cast_unsigned(),
                0,
                None,
                Some(&raw mut flags),
                None,
                Some(&raw mut sample),
            )?;
            if let Some(sample) = sample {
                times.push((sample.GetSampleTime()?, sample.GetSampleDuration()?));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0.cast_unsigned() != 0 {
                break;
            }
        }
        Ok(times)
    }
}
