use std::{
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow, bail, ensure};
use async_channel::Sender;
use cpal::{SampleFormat, SizedSample, traits::DeviceTrait};
use rtrb::Producer;

use super::control::{Control, Mode};
use crate::runtime::ports::Lost;

const UNSUPPORTED_FORMAT: &str = "Microphone format is unsupported. Change it in your system's sound settings, or choose another microphone in Settings.";
pub(super) const STALL: Duration = Duration::from_secs(2);
const NOT_SENDING: &str = "The microphone isn't sending audio.";
const STOPPED_SENDING: &str = "The microphone stopped sending audio.";
const CHECK_INPUT: &str = "Check the input device, or quit the app holding it, then try again.";

pub(super) struct CallbackState {
    pub(super) channels: NonZeroUsize,
    pub(super) producer: Producer<f32>,
    pub(super) control: Arc<Control>,
    pub(super) errors: Sender<cpal::Error>,
}

pub(super) fn rate_and_channels(
    format: &cpal::SupportedStreamConfig,
) -> anyhow::Result<(u32, NonZeroUsize)> {
    let rate = format.sample_rate();
    let channels = NonZeroUsize::new(usize::from(format.channels()))
        .context("Microphone has no input channels. Choose another microphone in Settings.")?;
    ensure!(rate > 0, UNSUPPORTED_FORMAT);
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
        _ => bail!(UNSUPPORTED_FORMAT),
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

pub(super) fn capture_callbacks<T>(
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
    } = state;
    let error_control = Arc::clone(&control);
    (
        move |data: &[T]| {
            if control.mode() != Mode::Recording {
                return;
            }
            let mono = data.chunks_exact(channels.get()).map(|frame| {
                frame
                    .iter()
                    .map(|sample| sample.to_sample::<f32>())
                    .sum::<f32>()
                    / channels.get() as f32
            });
            if let Ok(chunk) =
                producer.write_chunk_uninit((data.len() / channels).min(producer.slots()))
            {
                chunk.fill_from_iter(mono);
            }
        },
        move |error| {
            if error_control.mode() == Mode::Recording {
                handle_stream_error(error, &errors);
            }
        },
    )
}

fn handle_stream_error(error: cpal::Error, errors: &Sender<cpal::Error>) {
    if matches!(
        error.kind(),
        cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged
    ) {
        return;
    }
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A full single-slot lane already retains the first fatal error; a closed lane has no recording owner"
    )]
    let _ = errors.try_send(error);
}

pub(super) struct Pulse {
    since: Instant,
    heard: bool,
}

impl Pulse {
    pub(super) const fn new(now: Instant) -> Self {
        Self {
            since: now,
            heard: false,
        }
    }

    pub(super) const fn beat(&mut self, now: Instant) {
        self.since = now;
        self.heard = true;
    }

    pub(super) fn check(&self, now: Instant) -> anyhow::Result<Option<Lost>> {
        if now.saturating_duration_since(self.since) < STALL {
            return Ok(None);
        }
        ensure!(self.heard, "{NOT_SENDING} {CHECK_INPUT}");
        Ok(Some(Lost {
            notice: STOPPED_SENDING,
            failure: anyhow!("{STOPPED_SENDING} {CHECK_INPUT}"),
        }))
    }
}

impl From<cpal::Error> for Lost {
    fn from(error: cpal::Error) -> Self {
        let (notice, failure) = diagnose(error);
        Self { notice, failure }
    }
}

pub(super) fn microphone_error(error: cpal::Error) -> anyhow::Error {
    diagnose(error).1
}

fn diagnose(error: cpal::Error) -> (&'static str, anyhow::Error) {
    let explained = |cause, remedy| (cause, anyhow!("{cause} {remedy}"));
    match error.kind() {
        cpal::ErrorKind::DeviceNotAvailable => explained(
            "The microphone was disconnected.",
            "Reconnect it, or choose another microphone in Settings.",
        ),
        cpal::ErrorKind::DeviceBusy => explained(
            "Another app is using the microphone.",
            "Close that app, or choose another microphone in Settings.",
        ),
        cpal::ErrorKind::PermissionDenied => explained(
            "Speakeasy has no microphone access.",
            "Allow it in your system's privacy settings, then try again.",
        ),
        _ => (
            "The microphone failed.",
            anyhow!(
                "The microphone failed ({error}). Try recording again, or choose another microphone in Settings."
            ),
        ),
    }
}

#[cfg(test)]
mod tests;
