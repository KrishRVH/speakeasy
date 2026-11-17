use std::{
    iter,
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow, bail, ensure};
use async_channel::Sender;
use cpal::{SampleFormat, SizedSample, traits::DeviceTrait};
use rtrb::Producer;
use speakeasy_core::gesture::RECORDING_LIMIT;

use super::control::{Control, Mode};
use crate::runtime::ports::Lost;

pub(crate) const MAX_SAMPLE_RATE: u32 = 192_000;
const MAX_CHANNELS: usize = 32;
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
        started,
        limit,
    } = state;
    let error_control = Arc::clone(&control);
    let mut timeline = 0_usize;
    let mut owed = 0_usize;
    (
        move |data: &[T]| {
            let _publishing = control.publishing();
            if control.mode() != Mode::Recording || started.elapsed() >= RECORDING_LIMIT {
                return;
            }
            let frames = (data.len() / channels).min(limit.saturating_sub(timeline));
            timeline = timeline.saturating_add(frames);
            owed = owed.saturating_sub(queue(&mut producer, owed, iter::repeat(0.0)));
            let queued = if owed == 0 {
                let mono = data.chunks_exact(channels.get()).map(|frame| {
                    frame
                        .iter()
                        .map(|sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels.get() as f32
                });
                queue(&mut producer, frames, mono)
            } else {
                0
            };
            owed = owed.saturating_add(frames.saturating_sub(queued));
        },
        move |error| {
            let _publishing = error_control.publishing();
            if error_control.mode() == Mode::Recording {
                handle_stream_error(error, &errors);
            }
        },
    )
}

fn queue(producer: &mut Producer<f32>, frames: usize, samples: impl Iterator<Item = f32>) -> usize {
    producer
        .write_chunk_uninit(frames.min(producer.slots()))
        .map_or(0, |chunk| chunk.fill_from_iter(samples))
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
        let notice = match error.kind() {
            cpal::ErrorKind::DeviceNotAvailable => "The microphone was disconnected.",
            cpal::ErrorKind::DeviceBusy => "Another app is using the microphone.",
            cpal::ErrorKind::PermissionDenied => "Speakeasy has no microphone access.",
            _ => "The microphone failed.",
        };
        Self {
            notice,
            failure: microphone_error(error),
        }
    }
}

pub(super) fn microphone_error(error: cpal::Error) -> anyhow::Error {
    match error.kind() {
        cpal::ErrorKind::DeviceBusy => anyhow!(
            "Another app is using the microphone. Close that app, or choose another microphone in Settings."
        ),
        cpal::ErrorKind::PermissionDenied => anyhow!(
            "Speakeasy has no microphone access. Allow it in your system's privacy settings, then try again."
        ),
        cpal::ErrorKind::DeviceNotAvailable => {
            anyhow!(
                "The microphone was disconnected. Reconnect it, or choose another microphone in Settings."
            )
        },
        _ => anyhow!(
            "The microphone failed ({error}). Try recording again, or choose another microphone in Settings."
        ),
    }
}

#[cfg(test)]
mod tests;
