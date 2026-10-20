//! Validates the microphone's format and builds CPAL input streams with real-time callbacks that
//! never allocate or block. Stream errors are classified here for the capture thread.

use std::{num::NonZeroUsize, sync::Arc, time::Instant};

use anyhow::{Context, anyhow, bail, ensure};
use async_channel::{Receiver, Sender};
use cpal::{SampleFormat, SizedSample, traits::DeviceTrait};
use rtrb::Producer;
use speakeasy_core::gesture::RECORDING_LIMIT;

use super::{
    control::{Control, Mode},
    pcm::MAX_SAMPLE_RATE,
};

const MAX_CHANNELS: usize = 32;
pub(super) const UNSUPPORTED_FORMAT: &str = "Microphone format is unsupported. Change it in OS sound settings or choose another microphone in Settings.";

/// Everything the stream callbacks own for one capture.
pub(super) struct CallbackState {
    pub(super) channels: NonZeroUsize,
    pub(super) producer: Producer<f32>,
    pub(super) control: Arc<Control>,
    pub(super) errors: Sender<cpal::Error>,
    pub(super) started: Instant,
    pub(super) limit: usize,
}

pub(super) fn rate_and_channels(
    format: &cpal::SupportedStreamConfig,
) -> anyhow::Result<(u32, NonZeroUsize)> {
    let rate = format.sample_rate();
    let channels = NonZeroUsize::new(usize::from(format.channels()))
        .context("Microphone has no input channels. Choose another microphone in Settings.")?;
    ensure!(
        (1..=MAX_SAMPLE_RATE).contains(&rate) && channels.get() <= MAX_CHANNELS,
        UNSUPPORTED_FORMAT
    );
    Ok((rate, channels))
}

pub(super) fn build_stream(
    device: &cpal::Device,
    format: &cpal::SupportedStreamConfig,
    callbacks: CallbackState,
) -> anyhow::Result<cpal::Stream> {
    match format.sample_format() {
        SampleFormat::F32 => stream::<f32>(device, format.config(), callbacks),
        SampleFormat::I16 => stream::<i16>(device, format.config(), callbacks),
        SampleFormat::U16 => stream::<u16>(device, format.config(), callbacks),
        _ => bail!("Microphone sample format is unsupported. Choose a standard PCM microphone."),
    }
}

fn stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    callbacks: CallbackState,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let (mut data, error) = capture_callbacks::<T>(callbacks);
    device
        .build_input_stream(config, move |samples, _| data(samples), error, None)
        .map_err(microphone_error)
}

/// The callback pair for one capture, independent of any device so tests can replay native callback
/// ordering without opening a microphone.
fn capture_callbacks<T>(
    state: CallbackState,
) -> (
    impl FnMut(&[T]) + Send + 'static,
    impl FnMut(cpal::Error) + Send + 'static,
)
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let CallbackState {
        channels,
        mut producer,
        control,
        errors,
        started,
        limit,
    } = state;
    let error_control = control.clone();
    let mut queued_total = 0_usize;
    (
        move |data: &[T]| {
            let _publishing = control.publishing();
            if control.mode() != Mode::Recording || started.elapsed() >= RECORDING_LIMIT {
                return;
            }
            let frames = (data.len() / channels).min(limit.saturating_sub(queued_total));
            let queued = frames.min(producer.slots());
            let Ok(chunk) = producer.write_chunk_uninit(queued) else {
                control.mark_overflowed();
                return;
            };
            // One uninitialized chunk publishes the whole packet without scratch or allocation.
            chunk.fill_from_iter(data.chunks_exact(channels.get()).take(queued).map(|frame| {
                frame
                    .iter()
                    .map(|sample| sample.to_sample::<f32>())
                    .sum::<f32>()
                    / channels.get() as f32
            }));
            if queued > 0 && queued_total == 0 {
                control.mark_first_sample_queued();
            }
            queued_total = queued_total.saturating_add(queued);
            if queued < frames {
                control.mark_overflowed();
            }
        },
        move |error| {
            let _publishing = error_control.publishing();
            if error_control.mode() == Mode::Recording {
                handle_stream_error(error, &errors, &error_control);
            }
        },
    )
}

fn handle_stream_error(error: cpal::Error, errors: &Sender<cpal::Error>, control: &Control) {
    if is_recoverable(error.kind(), control.first_sample_queued()) {
        return;
    }
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A full single-slot lane already retains the first fatal error; a closed lane has no recording owner"
    )]
    let _ = errors.try_send(error);
}

fn is_recoverable(kind: cpal::ErrorKind, first_sample_queued: bool) -> bool {
    match kind {
        // WASAPI can report a discontinuity before its first packet, when no queued speech can be
        // lost. Later, an Xrun may have dropped words, so it fails the recording.
        cpal::ErrorKind::Xrun => !first_sample_queued,
        // Refused real-time priority and automatic rerouting leave the stream delivering.
        cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged => true,
        _ => false,
    }
}

pub(super) fn capture_failure(
    control: &Control,
    errors: &Receiver<cpal::Error>,
) -> anyhow::Result<()> {
    if control.overflowed() {
        bail!(
            "Recording buffer filled because capture could not keep up. Reduce system load and try again."
        );
    }
    if let Ok(error) = errors.try_recv() {
        return Err(microphone_error(error));
    }
    Ok(())
}

pub(super) fn microphone_error(error: cpal::Error) -> anyhow::Error {
    let guidance = match error.kind() {
        cpal::ErrorKind::DeviceBusy => {
            "Close other apps using the microphone or choose another microphone in Settings."
        },
        cpal::ErrorKind::PermissionDenied => "Check OS microphone permission and try again.",
        cpal::ErrorKind::DeviceNotAvailable => {
            "Reconnect the microphone or choose another in Settings."
        },
        cpal::ErrorKind::Xrun => {
            "Audio was interrupted during recording. Check your audio routing or reduce system load, then try again."
        },
        _ => "Try recording again or choose another microphone in Settings.",
    };
    anyhow!("Microphone failed ({:?}): {error} {guidance}", error.kind())
}

#[cfg(test)]
mod tests;
