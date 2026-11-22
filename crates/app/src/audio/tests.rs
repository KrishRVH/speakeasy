use anyhow::bail;
use speakeasy_core::machine::Cue;
use tokio::time::timeout;

use super::{stream::STALL, *};
use crate::testing::{PATIENCE, still_pending};

fn rms(dbfs: f32) -> f32 {
    10.0_f32.powf(dbfs / 20.0)
}

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 0.01
}

#[test]
fn speech_levels_sink_in_pauses_and_peak_at_the_loudest_recent_window() {
    let mut scale = SpeechScale::default();
    assert_eq!(
        scale.level(0.0, METER_INTERVAL).to_bits(),
        0.0_f32.to_bits()
    );
    assert!(near(scale.level(rms(-30.0), METER_INTERVAL), 1.0));
    assert!(near(scale.level(rms(-42.0), METER_INTERVAL), 0.25));
    assert_eq!(
        scale.level(rms(-60.0), METER_INTERVAL).to_bits(),
        0.0_f32.to_bits()
    );
    assert!(near(scale.level(1.0, METER_INTERVAL), 1.0));
    assert!(near(scale.level(rms(-32.0), METER_INTERVAL), 0.25));
}

#[test]
fn the_meter_rests_on_a_constant_offset_and_follows_speech_around_it() {
    let level = |samples: &mut dyn Iterator<Item = f32>| {
        let mut meter = LevelMeter::new();
        meter.since = Instant::now().checked_sub(METER_INTERVAL).unwrap();
        samples.for_each(|sample| meter.add(sample));
        meter.take_level().unwrap()
    };
    let speech = || {
        fixture::square_wave(1_000)
            .take(640)
            .map(|sample| f32::from(sample) / 32_768.0)
    };
    assert_eq!(
        level(&mut std::iter::repeat_n(0.05, 640)).to_bits(),
        0.0_f32.to_bits()
    );
    let spoken = level(&mut speech());
    assert!(
        spoken > 0.5,
        "The fixture must register as speech, or this test proves nothing"
    );
    assert!(near(
        level(&mut speech().map(|sample| sample + 0.05)),
        spoken
    ));
}

fn recorder(samples: &[f32], error: Option<cpal::ErrorKind>) -> anyhow::Result<Recorder<()>> {
    let (mut producer, ring) = RingBuffer::new(samples.len().max(1));
    producer.push_entire_slice(samples)?;
    let (errors, stream_errors) = async_channel::bounded(1);
    if let Some(kind) = error {
        errors.try_send(kind.into())?;
    }
    Ok(Recorder::new(
        (),
        ring,
        stream_errors,
        Instant::now(),
        16_000,
        RecognitionMode::Whole,
    ))
}

fn no_requests() -> Requests {
    Requests {
        cues: async_channel::unbounded().1,
        keep: async_channel::bounded(1).1,
        whole: async_channel::bounded(1).1,
    }
}

fn record_released(recorder: Recorder<()>) -> Outcome {
    let (events, _reports) = async_channel::bounded(64);
    let control = Control::default();
    control.finish();
    recorder
        .record(SessionId::FIRST, &events, &control, &no_requests())
        .outcome
}

fn record_constant(level: f32, samples: usize) -> anyhow::Result<Option<Sealed>> {
    match record_released(recorder(&vec![level; samples], None)?) {
        Outcome::Finished(sealed) => sealed,
        Outcome::Lost(..) => bail!("A healthy microphone was reported lost"),
    }
}

fn speech() -> impl Iterator<Item = i16> {
    fixture::square_wave(2_000).take(16_000)
}

fn spoken() -> Vec<f32> {
    speech()
        .map(|sample| f32::from(sample) / 32_768.0)
        .collect()
}

fn stalled_since(arrived: bool) -> anyhow::Result<Pulse> {
    let opened = Instant::now()
        .checked_sub(STALL)
        .context("The monotonic clock started less than a stall ago")?;
    let mut pulse = Pulse::new(opened);
    if arrived {
        pulse.beat(opened);
    }
    Ok(pulse)
}

#[test]
fn only_exact_digital_silence_long_enough_to_seal_names_the_microphone() -> anyhow::Result<()> {
    let failure = record_constant(0.0, 8_000)
        .err()
        .context("A microphone that sent only zeros read as no speech")?;
    assert!(failure.is::<SilentMicrophone>());
    assert!(
        record_constant(f32::NAN, 8_000).is_err_and(|failure| failure.is::<SilentMicrophone>())
    );
    assert!(
        record_constant(1e-6, 8_000)?.is_none(),
        "Quiet input below one PCM16 step still came from a live microphone"
    );
    assert!(record_constant(0.0, 1_600)?.is_none());
    Ok(())
}

#[test]
fn a_microphone_lost_after_speech_seals_what_arrived() -> anyhow::Result<()> {
    let recorded = recorder(&spoken(), Some(cpal::ErrorKind::DeviceNotAvailable))?;
    let Outcome::Lost(sealed, lost) = record_released(recorded) else {
        bail!("Speech captured before the microphone was lost was discarded");
    };
    assert_eq!(wav_duration(&sealed.wav)?, Duration::from_secs(1));
    assert_eq!(lost.notice, "The microphone was disconnected.");
    Ok(())
}

#[test]
fn a_microphone_lost_before_speech_fails_with_its_named_error() -> anyhow::Result<()> {
    for samples in [Vec::new(), vec![0.0; 8_000], vec![1e-6; 8_000]] {
        let recorded = recorder(&samples, Some(cpal::ErrorKind::DeviceNotAvailable))?;
        let Outcome::Finished(Err(failure)) = record_released(recorded) else {
            bail!("A microphone lost before any speech did not fail");
        };
        assert!(!failure.is::<SilentMicrophone>());
        assert!(
            failure
                .to_string()
                .starts_with("The microphone was disconnected. Reconnect it"),
            "{failure}"
        );
    }
    Ok(())
}

#[test]
fn a_microphone_that_never_sends_audio_is_named_instead_of_hearing_no_speech() -> anyhow::Result<()>
{
    let mut silent = recorder(&[], None)?;
    silent.pulse = stalled_since(false)?;
    let Outcome::Finished(Err(failure)) = record_released(silent) else {
        bail!("A microphone that never sent audio read as no speech");
    };
    assert!(
        failure
            .to_string()
            .starts_with("The microphone isn't sending audio."),
        "{failure}"
    );
    Ok(())
}

#[test]
fn a_microphone_that_stops_sending_after_speech_keeps_what_arrived() -> anyhow::Result<()> {
    let mut stopped = recorder(&[], None)?;
    speech().for_each(|sample| stopped.pcm.push(sample));
    stopped.pulse = stalled_since(true)?;
    let Outcome::Lost(sealed, lost) = record_released(stopped) else {
        bail!("Speech before the microphone stopped sending audio was not kept");
    };
    assert_eq!(wav_duration(&sealed.wav)?, Duration::from_secs(1));
    assert_eq!(lost.notice, "The microphone stopped sending audio.");
    Ok(())
}

async fn pending_until_released(
    retirement: impl Future<Output = ()>,
    release: &Sender<()>,
) -> anyhow::Result<bool> {
    tokio::pin!(retirement);
    let pending = still_pending(retirement.as_mut()).await;
    release.send(()).await?;
    timeout(PATIENCE, retirement).await?;
    Ok(pending)
}

#[tokio::test]
async fn sealed_audio_is_reported_before_device_teardown_and_retirement_waits() -> anyhow::Result<()>
{
    struct Device {
        entered: Sender<()>,
        release: Receiver<()>,
    }
    impl Drop for Device {
        fn drop(&mut self) {
            self.entered.send_blocking(()).unwrap();
            self.release.recv_blocking().unwrap_or_default();
        }
    }
    let (events, audio) = async_channel::bounded(1);
    let (entered, teardown) = async_channel::bounded(1);
    let (release, waiting) = async_channel::bounded(1);
    let capture = Capture::spawn(SessionId::FIRST, events, move |_, _, _| {
        Ok(Captured {
            outcome: Outcome::Finished(Ok(Some(Sealed {
                wav: vec![1, 2],
                trimmed: Duration::ZERO,
                audible: Duration::ZERO,
                partition: None,
            }))),
            device: Device {
                entered,
                release: waiting,
            },
            tail: None,
        })
    })?;
    let tearing_down = timeout(PATIENCE, teardown.recv()).await;
    let report = timeout(PATIENCE, audio.recv()).await;
    let pending = pending_until_released(capture.retire(), &release).await?;
    tearing_down??;
    assert!(
        matches!(report?, Ok(CaptureEvent::Finished(SessionId::FIRST, Ok(Some(sealed)))) if *sealed.wav == [1, 2])
    );
    assert!(pending, "Retirement acknowledged before device teardown");
    Ok(())
}

#[test]
fn pcm_chunks_wrap_clamp_invalid_samples_and_enforce_the_limit() -> anyhow::Result<()> {
    let (mut producer, mut ring) = RingBuffer::new(4);
    producer.push_entire_slice(&[0.0, 0.0, 0.0])?;
    ring.read_chunk(3)?.commit_all();
    producer.push_entire_slice(&[0.5, f32::NAN, 2.0, -2.0])?;
    let mut pcm = Pcm16::with_reservation(0, 3);
    let mut meter = LevelMeter::new();
    assert!(consume_pcm(&mut ring, &mut pcm, &mut meter));
    assert!((meter.power.mean_square() - 1.0 / 6.0).abs() < 1e-12);
    let expected = [16383_i16, 0, 32767]
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .collect::<Vec<_>>();
    assert_eq!(pcm.audio(), expected);
    assert!(ring.is_empty());
    assert_eq!(producer.slots(), 4);
    Ok(())
}

#[test]
fn a_cue_noticed_after_its_first_samples_arrive_still_counts_only_by_its_voice_band() {
    let loud_show_cue: Vec<u8> = fixture::loud_cue(Cue::Show)
        .chain(std::iter::repeat_n(0, 96_000))
        .collect();
    let retained = |noticed: bool| {
        let (cues, notices) = async_channel::unbounded();
        if noticed {
            cues.try_send(()).unwrap();
        }
        let mut speech = Audibility::new(48_000);
        let (_sender, keep) = async_channel::bounded(1);
        let mut keeping = checkpoint::Keeping::new(&keep);
        keeping.classify(&mut speech, &loud_show_cue, &notices);
        speech.retained(loud_show_cue.len())
    };
    assert!(
        retained(false).is_some(),
        "An unnoticed loud cue must count as speech, or this test proves nothing"
    );
    assert_eq!(retained(true), None);
}
