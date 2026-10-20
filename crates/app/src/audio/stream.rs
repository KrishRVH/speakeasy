//! CPAL input streams and their real-time callbacks, which never allocate or block. Stream errors
//! are classified here for the capture thread.

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
        .context("Unsupported microphone format: no input channels")?;
    ensure!(
        (1..=MAX_SAMPLE_RATE).contains(&rate) && channels.get() <= MAX_CHANNELS,
        "Unsupported microphone format"
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
mod tests {
    use rtrb::{Consumer, RingBuffer};

    use super::*;

    /// A callback state over a fresh ring, with the ends a capture thread would keep.
    fn callback_fixture(
        channels: usize,
        capacity: usize,
        limit: usize,
    ) -> (
        CallbackState,
        Consumer<f32>,
        Receiver<cpal::Error>,
        Arc<Control>,
    ) {
        let (producer, ring) = RingBuffer::new(capacity);
        let (errors, received) = async_channel::bounded(1);
        let control = Arc::new(Control::default());
        let state = CallbackState {
            channels: NonZeroUsize::new(channels).unwrap(),
            producer,
            control: control.clone(),
            errors,
            started: Instant::now(),
            limit,
        };
        (state, ring, received, control)
    }

    #[test]
    fn callbacks_after_finish_cannot_publish_samples_or_teardown_errors() {
        let (state, mut ring, errors, control) = callback_fixture(1, 10, 10);
        let (mut data, mut error) = capture_callbacks::<f32>(state);
        data(&[0.25; 3]);
        control.finish();
        control.quiesce();
        data(&[0.5; 3]);
        error(cpal::ErrorKind::DeviceNotAvailable.into());
        assert_eq!(ring.slots(), 3);
        let mut samples = [0.0; 3];
        ring.pop_entire_slice(&mut samples).unwrap();
        assert_eq!(samples, [0.25; 3]);
        assert!(errors.try_recv().is_err());
    }

    #[test]
    fn startup_xrun_is_allowed_but_an_interruption_after_samples_is_fatal() -> anyhow::Result<()> {
        let (state, mut ring, errors, control) = callback_fixture(2, 4, 4);
        let (mut data, mut error) = capture_callbacks::<f32>(state);
        error(cpal::ErrorKind::Xrun.into());
        assert!(capture_failure(&control, &errors).is_ok());
        data(&[0.25, 0.75]);
        assert_eq!(ring.pop()?, 0.5);
        error(cpal::ErrorKind::Xrun.into());
        let failure = capture_failure(&control, &errors)
            .err()
            .context("An interruption after samples must not silently discard speech")?;
        assert!(failure.to_string().contains("Xrun"));
        Ok(())
    }

    #[test]
    fn callback_overflow_is_fatal_but_recording_limit_and_cancellation_do_not_overflow()
    -> anyhow::Result<()> {
        for limit in [2, 3] {
            let (state, mut ring, errors, control) = callback_fixture(1, 2, limit);
            let (mut data, _) = capture_callbacks::<f32>(state);
            data(&[0.25, 0.5, 0.75]);
            assert_eq!(capture_failure(&control, &errors).is_err(), limit == 3);
            assert_eq!(ring.pop()?, 0.25);
            assert_eq!(ring.pop()?, 0.5);
            control.cancel();
            data(&[1.0]);
            assert!(ring.pop().is_err());
        }
        Ok(())
    }

    #[test]
    fn recoverable_stream_notifications_do_not_abort_recording() {
        let (errors, received) = async_channel::bounded(1);
        for kind in [
            cpal::ErrorKind::RealtimeDenied,
            cpal::ErrorKind::DeviceChanged,
        ] {
            for first_sample_queued in [false, true] {
                let control = Control::default();
                if first_sample_queued {
                    control.mark_first_sample_queued();
                }
                handle_stream_error(kind.into(), &errors, &control);
                assert!(received.try_recv().is_err(), "{kind} aborted capture");
                assert!(capture_failure(&Control::default(), &received).is_ok());
            }
        }
    }

    #[test]
    fn fatal_stream_errors_keep_the_first_cause_without_blocking() -> anyhow::Result<()> {
        let (errors, received) = async_channel::bounded(1);
        let before_samples = Control::default();
        handle_stream_error(
            cpal::Error::with_message(cpal::ErrorKind::DeviceBusy, "fixture device is busy"),
            &errors,
            &before_samples,
        );
        for _ in 0..10 {
            handle_stream_error(cpal::ErrorKind::Xrun.into(), &errors, &before_samples);
            let lost = cpal::ErrorKind::DeviceNotAvailable.into();
            handle_stream_error(lost, &errors, &before_samples);
        }
        let error = capture_failure(&Control::default(), &received)
            .err()
            .context("fatal driver error must abort recording")?
            .to_string();
        assert!(error.contains("DeviceBusy"));
        assert!(error.contains("fixture device is busy"));
        assert!(error.contains("Close other apps"));
        assert!(!error.contains("buffer"));
        Ok(())
    }

    #[test]
    fn device_loss_and_backend_failures_abort_recording() -> anyhow::Result<()> {
        for kind in [
            cpal::ErrorKind::DeviceNotAvailable,
            cpal::ErrorKind::StreamInvalidated,
            cpal::ErrorKind::PermissionDenied,
            cpal::ErrorKind::BackendError,
        ] {
            let (errors, received) = async_channel::bounded(1);
            handle_stream_error(
                cpal::Error::with_message(kind, "fixture driver failure"),
                &errors,
                &Control::default(),
            );
            let error = capture_failure(&Control::default(), &received)
                .err()
                .context("fatal driver error must abort recording")?
                .to_string();
            assert!(error.contains(&format!("{kind:?}")));
            assert!(error.contains("fixture driver failure"));
        }
        Ok(())
    }

    #[test]
    fn recording_buffer_overflow_has_its_own_actionable_error() -> anyhow::Result<()> {
        let (_errors, received) = async_channel::bounded(1);
        let control = Control::default();
        control.mark_overflowed();
        let error = capture_failure(&control, &received)
            .err()
            .context("application ring overflow must abort recording")?
            .to_string();
        assert!(error.contains("Recording buffer filled"));
        assert!(error.contains("Reduce system load"));
        Ok(())
    }

    #[test]
    #[ignore = "opt-in synthetic release profiling; never opens a microphone"]
    fn profile_audio_pipeline() -> anyhow::Result<()> {
        use std::hint::black_box;

        use crate::audio::{LevelMeter, consume_pcm, pcm::Pcm16};

        for channels in [1, 2, 8] {
            let mut timings = Vec::new();
            for _ in 0..21 {
                let (state, mut ring, errors, control) =
                    callback_fixture(channels, 48_000, 480 * 3000);
                let (mut data, _) = capture_callbacks::<f32>(state);
                let input = vec![0.125; 480 * channels];
                let mut pcm = Pcm16::with_reservation(480);
                let mut meter = LevelMeter::new();
                let started = Instant::now();
                for _ in 0..3000 {
                    data(black_box(&input));
                    consume_pcm(&mut ring, &mut pcm, &mut meter, 480);
                    black_box(&meter);
                    black_box(pcm.audio());
                    pcm.clear();
                }
                timings.push(started.elapsed().as_secs_f64() * 1e6 / 3000.0);
                capture_failure(&control, &errors)?;
            }
            timings.sort_by(f64::total_cmp);
            eprintln!(
                "audio_pipeline: channels={channels} frames=480 median_us={:.3} p95_us={:.3} runs=21 batches_per_run=3000",
                timings[10], timings[19]
            );
        }
        Ok(())
    }
}
