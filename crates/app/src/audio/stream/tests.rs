use async_channel::Receiver;
use rtrb::{Consumer, RingBuffer};

use super::*;

fn callback_fixture(
    channels: usize,
    capacity: usize,
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
    };
    (state, ring, received, control)
}

fn drain(ring: &mut Consumer<f32>) -> Vec<f32> {
    let mut samples = vec![0.0; ring.slots()];
    ring.pop_entire_slice(&mut samples).unwrap();
    samples
}

#[test]
fn glitches_and_rerouting_never_stop_a_recording_before_or_after_its_first_sample() {
    let (state, mut ring, errors, _control) = callback_fixture(2, 4);
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
#[ignore = "opt-in synthetic release profiling; never opens a microphone"]
fn profile_audio_pipeline() {
    use std::{hint::black_box, time::Instant};

    use crate::audio::{LevelMeter, consume_pcm, pcm::Pcm16};

    for channels in [1, 2, 8] {
        let mut timings = Vec::new();
        for _ in 0..21 {
            let (state, mut ring, _errors, _control) = callback_fixture(channels, 48_000);
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
