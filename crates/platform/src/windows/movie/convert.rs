use std::mem::ManuallyDrop;

use anyhow::{Context as _, ensure};
use windows::Win32::{
    Media::MediaFoundation::{
        CLSID_VideoProcessorMFT, IMFMediaType, IMFSample, IMFTransform, MF_E_ATTRIBUTENOTFOUND,
        MF_E_TRANSFORM_NEED_MORE_INPUT, MF_TRANSFORM_ASYNC, MF_XVP_DISABLE_FRC,
        MFCreateAlignedMemoryBuffer, MFCreateSample, MFT_MESSAGE_COMMAND_DRAIN,
        MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING,
        MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
        MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    },
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
};

pub(super) struct Converter(IMFTransform);

struct Output(MFT_OUTPUT_DATA_BUFFER);

impl Converter {
    pub(super) fn new(
        input: &IMFMediaType,
        output: &IMFMediaType,
    ) -> anyhow::Result<(Self, IMFMediaType)> {
        unsafe {
            let native: IMFTransform =
                CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER)?;
            let converter = Self(native);
            let attributes = converter.0.GetAttributes()?;
            let asynchronous = match attributes.GetUINT32(&MF_TRANSFORM_ASYNC) {
                Ok(value) => value != 0,
                Err(error) if error.code() == MF_E_ATTRIBUTENOTFOUND => false,
                Err(error) => return Err(error.into()),
            };
            ensure!(
                !asynchronous,
                "Video conversion requires synchronous processing"
            );
            attributes.SetUINT32(&MF_XVP_DISABLE_FRC, 1)?;
            converter.0.SetInputType(0, input, 0)?;
            converter.0.SetOutputType(0, output, 0)?;
            converter
                .0
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            converter
                .0
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let format = converter.0.GetOutputCurrentType(0)?;
            Ok((converter, format))
        }
    }

    pub(super) fn frame(&self, input: &IMFSample) -> anyhow::Result<IMFSample> {
        unsafe { self.0.ProcessInput(0, input, 0)? };
        let sample = self
            .output()?
            .context("Video conversion delayed its frame")?;
        ensure!(
            self.output()?.is_none(),
            "Video conversion duplicated its frame"
        );

        unsafe {
            ensure!(
                sample.GetSampleTime()? == input.GetSampleTime()?
                    && sample.GetSampleDuration()? == input.GetSampleDuration()?,
                "Video conversion changed its sample clock"
            );
        }
        Ok(sample)
    }

    pub(super) fn finish(&self) -> anyhow::Result<()> {
        unsafe {
            self.0.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.0.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        ensure!(
            self.output()?.is_none(),
            "Video conversion retained an unsubmitted frame"
        );
        Ok(())
    }

    fn output(&self) -> anyhow::Result<Option<IMFSample>> {
        let info = unsafe { self.0.GetOutputStreamInfo(0)? };
        let native_allocation = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
            | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
            .cast_unsigned();
        let supplied = if info.dwFlags & native_allocation == 0 {
            ensure!(
                (1..=64 << 20).contains(&info.cbSize),
                "Video conversion buffer is out of range"
            );

            unsafe {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateAlignedMemoryBuffer(
                    info.cbSize,
                    info.cbAlignment.saturating_sub(1),
                )?)?;
                Some(sample)
            }
        } else {
            None
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "The generated COM output structure requires ManuallyDrop fields; Output releases both references on every path"
        )]
        let mut output = Output(MFT_OUTPUT_DATA_BUFFER {
            pSample: ManuallyDrop::new(supplied),
            ..MFT_OUTPUT_DATA_BUFFER::default()
        });
        let mut status = 0;

        let processed = unsafe {
            self.0
                .ProcessOutput(0, std::slice::from_mut(&mut output.0), &raw mut status)
        };
        match processed {
            Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
            result => result?,
        }
        ensure!(
            status == 0 && output.0.dwStatus == 0,
            "Video conversion changed its output contract"
        );
        output
            .0
            .pSample
            .take()
            .map(Some)
            .context("Video conversion returned no frame")
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.0.pSample);
            ManuallyDrop::drop(&mut self.0.pEvents);
        }
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        for message in [MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_END_STREAMING] {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Teardown releases pending conversion buffers even after a failed native call; the recording owner already reports that failure"
            )]
            let _ = unsafe { self.0.ProcessMessage(message, 0) };
        }
    }
}
