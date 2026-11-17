use async_channel::Receiver;
use rtrb::{Consumer, RingBuffer};

use super::*;

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
        control: Arc::clone(&control),
        errors,
        started: Instant::now(),
        limit,
    };
    (state, ring, received, control)
}

fn drain(ring: &mut Consumer<f32>) -> Vec<f32> {
    let mut samples = vec![0.0; ring.slots()];
    ring.pop_entire_slice(&mut samples).unwrap();
    samples
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
    assert_eq!(drain(&mut ring), [0.25; 3]);
    assert!(errors.try_recv().is_err());
}

#[test]
fn glitches_and_rerouting_never_stop_a_recording_before_or_after_its_first_sample() {
    let (state, mut ring, errors, _control) = callback_fixture(2, 4, 4);
    let (mut data, mut error) = capture_callbacks::<f32>(state);
    let glitches = [
        cpal::ErrorKind::Xrun,
        cpal::ErrorKind::RealtimeDenied,
        cpal::ErrorKind::DeviceChanged,
    ];
    for kind in glitches {
        error(kind.into());
    }
    data(&[0.25, 0.75]);
    for kind in glitches {
        error(kind.into());
    }
    data(&[0.5, 0.5]);
    assert!(
        errors.try_recv().is_err(),
        "A glitch reached the recording as a lost microphone"
    );
    assert_eq!(drain(&mut ring), [0.5, 0.5]);
}

#[test]
fn ring_overflow_becomes_silence_of_its_exact_length_within_the_limit() {
    let (state, mut ring, _errors, _control) = callback_fixture(1, 2, 10);
    let (mut data, _) = capture_callbacks::<f32>(state);
    data(&[0.25, 0.5, 0.75]);
    assert_eq!(drain(&mut ring), [0.25, 0.5]);
    data(&[1.0]);
    assert_eq!(
        drain(&mut ring),
        [0.0, 1.0],
        "The overflowed frame must hold its place as silence"
    );

    let (state, mut ring, _errors, _control) = callback_fixture(1, 2, 4);
    let (mut data, _) = capture_callbacks::<f32>(state);
    data(&[0.25, 0.5, 0.75, 0.875, 0.9]);
    assert_eq!(drain(&mut ring), [0.25, 0.5]);
    data(&[1.0]);
    assert_eq!(drain(&mut ring), [0.0, 0.0]);
    data(&[1.0]);
    assert!(ring.is_empty(), "Silence filled past the recording limit");

    let (state, mut ring, _errors, control) = callback_fixture(1, 2, 10);
    let (mut data, _) = capture_callbacks::<f32>(state);
    data(&[0.25, 0.5, 0.75]);
    assert_eq!(drain(&mut ring), [0.25, 0.5]);
    control.cancel();
    data(&[1.0]);
    assert!(ring.is_empty(), "A cancelled recording queued silence");
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
        assert!(
            error.to_string().contains("another microphone in Settings"),
            "{channels} channels at {rate} Hz has no remedy"
        );
    }
    Ok(())
}

#[test]
fn a_lost_stream_keeps_its_first_cause_without_blocking() -> anyhow::Result<()> {
    let (errors, received) = async_channel::bounded(1);
    handle_stream_error(
        cpal::Error::with_message(cpal::ErrorKind::DeviceBusy, "fixture device is busy"),
        &errors,
    );
    handle_stream_error(cpal::ErrorKind::DeviceNotAvailable.into(), &errors);
    let lost = Lost::from(received.try_recv()?);
    assert_eq!(lost.notice, "Another app is using the microphone.");
    let failure = lost.failure.to_string();
    assert!(
        failure.starts_with("Another app is using the microphone."),
        "{failure}"
    );
    assert!(received.try_recv().is_err());
    Ok(())
}

#[test]
fn a_lost_stream_names_its_cause_briefly_and_its_remedy_in_full() -> anyhow::Result<()> {
    for (kind, notice) in [
        (
            cpal::ErrorKind::DeviceNotAvailable,
            "The microphone was disconnected.",
        ),
        (
            cpal::ErrorKind::PermissionDenied,
            "Speakeasy has no microphone access.",
        ),
        (cpal::ErrorKind::StreamInvalidated, "The microphone failed."),
        (cpal::ErrorKind::BackendError, "The microphone failed."),
    ] {
        let (errors, received) = async_channel::bounded(1);
        handle_stream_error(
            cpal::Error::with_message(kind, "fixture driver failure"),
            &errors,
        );
        let lost = Lost::from(received.try_recv()?);
        assert_eq!(lost.notice, notice);
        let failure = lost.failure.to_string();
        assert!(!failure.contains(&format!("{kind:?}")), "{failure}");
        let explained = matches!(
            kind,
            cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::PermissionDenied
        );
        assert_eq!(
            failure.contains("fixture driver failure"),
            !explained,
            "{failure}"
        );
        assert!(
            failure.ends_with(", then try again.") || failure.ends_with("in Settings."),
            "{failure}"
        );
    }
    Ok(())
}

#[test]
fn a_stream_silent_from_its_start_fails_and_one_that_stops_is_lost() -> anyhow::Result<()> {
    let opened = Instant::now();
    let after = |millis| opened + Duration::from_millis(millis);
    let mut pulse = Pulse::new(opened);
    assert!(pulse.check(after(1_999))?.is_none());
    let failure = pulse
        .check(after(2_000))
        .err()
        .context("A microphone that never sent audio was not named")?
        .to_string();
    assert!(
        failure.starts_with("The microphone isn't sending audio. Check the input device"),
        "{failure}"
    );

    pulse.beat(after(2_500));
    assert!(pulse.check(after(4_499))?.is_none());
    let lost = pulse
        .check(after(4_500))?
        .context("A microphone that stopped sending audio was not lost")?;
    assert_eq!(lost.notice, "The microphone stopped sending audio.");
    assert!(
        lost.failure
            .to_string()
            .ends_with("Check the input device, or quit the app holding it, then try again.")
    );
    Ok(())
}

#[test]
#[ignore = "opt-in synthetic release profiling; never opens a microphone"]
fn profile_audio_pipeline() {
    use std::hint::black_box;

    use crate::audio::{LevelMeter, consume_pcm, pcm::Pcm16};

    for channels in [1, 2, 8] {
        let mut timings = Vec::new();
        for _ in 0..21 {
            let (state, mut ring, _errors, _control) =
                callback_fixture(channels, 48_000, 480 * 3000);
            let (mut data, _) = capture_callbacks::<f32>(state);
            let input = vec![0.125; 480 * channels];
            let mut pcm = Pcm16::with_reservation(480, 480);
            let mut meter = LevelMeter::new();
            let started = Instant::now();
            for _ in 0..3000 {
                data(black_box(&input));
                consume_pcm(&mut ring, &mut pcm, &mut meter);
                black_box(&meter);
                black_box(pcm.audio());
                pcm.clear();
            }
            timings.push(started.elapsed().as_secs_f64() * 1e6 / 3000.0);
        }
        timings.sort_by(f64::total_cmp);
        eprintln!(
            "audio_pipeline: channels={channels} frames=480 median_us={:.3} p95_us={:.3} runs=21 batches_per_run=3000",
            timings[10], timings[19]
        );
    }
}
