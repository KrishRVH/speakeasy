//! Drives format validation, the real-time callbacks, and stream-error classification over fresh
//! rings, with no device or driver, and times the callback-to-consumer path in an opt-in release
//! profile.

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
fn formats_beyond_the_capture_bounds_are_refused_with_a_remedy() -> anyhow::Result<()> {
    let format = |channels, rate| {
        cpal::SupportedStreamConfig::new(
            channels,
            rate,
            cpal::SupportedBufferSize::Unknown,
            SampleFormat::F32,
        )
    };
    assert_eq!(rate_and_channels(&format(1, 1))?, (1, NonZeroUsize::MIN));
    assert_eq!(
        rate_and_channels(&format(32, MAX_SAMPLE_RATE))?,
        (MAX_SAMPLE_RATE, NonZeroUsize::new(32).unwrap())
    );
    for (channels, rate) in [(0, 48_000), (33, 48_000), (1, 0), (1, MAX_SAMPLE_RATE + 1)] {
        let error = rate_and_channels(&format(channels, rate))
            .err()
            .context("An unsupported format was accepted")?;
        assert_eq!(
            error.to_string(),
            UNSUPPORTED_FORMAT,
            "{channels} channels at {rate} Hz"
        );
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
            let (state, mut ring, errors, control) = callback_fixture(channels, 48_000, 480 * 3000);
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
