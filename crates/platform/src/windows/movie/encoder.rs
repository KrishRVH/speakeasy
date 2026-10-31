//! Native objects stay on the capture thread. Field order releases the writer, shuts down its
//! sink, closes its stream, and only then leaves Media Foundation and the COM apartment.

use std::{marker::PhantomData, path::Path, time::Duration};

use anyhow::{Context as _, ensure};
use windows::{
    Win32::{
        Media::MediaFoundation::{
            IMFAttributes, IMFMediaBuffer, IMFMediaSink, IMFMediaType, IMFSample, IMFSinkWriter,
            MF_LOW_LATENCY, MF_MPEG4SINK_MAX_CODED_SEQUENCES_PER_FRAGMENT,
            MF_MPEG4SINK_MIN_FRAGMENT_DURATION, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE,
            MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
            MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
            MF_SINK_WRITER_DISABLE_THROTTLING, MF_SINK_WRITER_STATISTICS, MF_VERSION,
            MFCreateAttributes, MFCreateFMPEG4MediaSink, MFCreateMediaType, MFCreateMemoryBuffer,
            MFCreateSample, MFCreateSinkWriterFromMediaSink, MFMediaType_Video, MFSTARTUP_FULL,
            MFShutdown, MFStartup, MFVideoFormat_H264, MFVideoFormat_RGB32,
            MFVideoInterlace_Progressive,
        },
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
    },
    core::{GUID, Interface as _},
};

use super::{canvas::Canvas, file};
use crate::Frame;

const TICKS: u64 = 10_000_000;
const QUEUED_BYTES: u32 = 64 << 20;

pub(super) struct Encoder {
    writer: IMFSinkWriter,
    _sink: Sink,
    stream: file::Stream,
    canvas: Canvas,
    synced: Duration,
    last: Option<Duration>,
    _foundation: Foundation,
}

struct Sink(IMFMediaSink);

struct Foundation {
    // COM apartment cleanup must run on its initializing thread, including on partial open failure.
    _thread: PhantomData<*mut ()>,
}

impl Foundation {
    fn start() -> anyhow::Result<Self> {
        // SAFETY: this owned capture thread uses the MTA. Every successful initialization has a
        // matching uninitialize on this thread, after its Media Foundation objects are released.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        // SAFETY: balanced with MFShutdown after every object this startup creates is released.
        if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            // SAFETY: the preceding CoInitializeEx succeeded on this thread.
            unsafe { CoUninitialize() };
            return Err(error.into());
        }
        Ok(Self {
            _thread: PhantomData,
        })
    }
}

impl Drop for Foundation {
    fn drop(&mut self) {
        // SAFETY: field/local drop order has released every native object before this guard.
        unsafe {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Teardown has no observer on partial initialization or unwind; the checkpoint retains any partial movie"
            )]
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

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

impl Encoder {
    pub(super) fn open(path: &Path, canvas: Canvas, limit: u64) -> anyhow::Result<Self> {
        let foundation = Foundation::start()?;
        let stream = file::create(path, limit)?;
        let size = canvas.size();
        let output = media(&MFVideoFormat_H264, size)?;
        // SAFETY: the attributes and media types are owned, initialized COM objects. Values are
        // documented native encodings; none borrows Rust memory beyond these calls.
        let (sink, writer) = unsafe {
            output.SetUINT32(&MF_MT_AVG_BITRATE, 8_000_000)?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, 100)?;
            let sink = Sink(MFCreateFMPEG4MediaSink(&stream.native, &output, None)?);
            let fragments: IMFAttributes = sink.0.cast()?;
            fragments.SetUINT64(&MF_MPEG4SINK_MIN_FRAGMENT_DURATION, TICKS)?;
            fragments.SetUINT32(&MF_MPEG4SINK_MAX_CODED_SEQUENCES_PER_FRAGMENT, 1)?;
            let mut attributes = None;
            MFCreateAttributes(&raw mut attributes, 2)?;
            let attributes = attributes.context("Video writer attributes are unavailable")?;
            // CPU-backed RGB32 frames use the system software path. Hardware transform discovery
            // delays the first frame; a GPU encoder needs a D3D surface input and device manager.
            attributes.SetUINT32(&MF_LOW_LATENCY, 1)?;
            attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            let writer = MFCreateSinkWriterFromMediaSink(&sink.0, &attributes)?;
            let input = media(&MFVideoFormat_RGB32, size)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, size[0].saturating_mul(4))?;
            input.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            writer.SetInputMediaType(0, &input, None)?;
            writer.BeginWriting()?;
            (sink, writer)
        };
        Ok(Self {
            writer,
            _sink: sink,
            stream,
            canvas,
            synced: Duration::ZERO,
            last: None,
            _foundation: foundation,
        })
    }

    pub(super) fn write(&mut self, frame: &Frame, at: Duration) -> anyhow::Result<()> {
        if at.saturating_sub(self.synced) >= Duration::from_secs(1) {
            // SAFETY: the byte stream stays open until finalization, and Commit syncs its file.
            unsafe { self.stream.native.Flush()? };
            self.synced = at;
        }
        let pixels = self.canvas.paint(frame)?;
        submit(&self.writer, pixels, at)?;
        self.last = Some(at);
        Ok(())
    }

    pub(super) fn finish(self, through: Option<Duration>) -> anyhow::Result<()> {
        if let (Some(last), Some(through)) = (self.last, through) {
            let tail = through.saturating_sub(super::INTERVAL);
            if tail > last {
                submit(&self.writer, self.canvas.held(), tail)?;
            }
        }
        // SAFETY: all input submissions ended. Finalize closes the stream; no later Flush may
        // touch it. Field teardown still shuts the sink down when Finalize fails.
        unsafe { self.writer.Finalize()? };
        self.stream.sync.sync_data()?;
        Ok(())
    }
}

fn submit(writer: &IMFSinkWriter, pixels: &[u8], at: Duration) -> anyhow::Result<()> {
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
    let sample = sample(pixels, at)?;
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

fn sample(pixels: &[u8], at: Duration) -> anyhow::Result<IMFSample> {
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
        sample.SetSampleDuration(i64::try_from(TICKS / 30)?)?;
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
