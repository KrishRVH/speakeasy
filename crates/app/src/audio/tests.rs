use std::{sync::PoisonError, time::SystemTime};

use anyhow::{bail, ensure};
use speakeasy_core::machine::Cue;
use tokio::time::timeout;

use super::{pcm::SAMPLE_BYTES, stream::STALL, *};
use crate::{
    show::{Keeper, Kept},
    sounds,
    testing::{PATIENCE, still_pending},
};

const METER_WINDOW: Duration = Duration::from_millis(32);

fn rms(dbfs: f32) -> f32 {
    10.0_f32.powf(dbfs / 20.0)
}

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 0.01
}

#[test]
fn speech_levels_sink_in_pauses_and_peak_at_the_loudest_recent_window() {
    let mut scale = SpeechScale::default();
    assert_eq!(scale.level(0.0, METER_WINDOW).to_bits(), 0.0_f32.to_bits());
    assert!(near(scale.level(rms(-30.0), METER_WINDOW), 1.0));
    assert!(near(scale.level(rms(-42.0), METER_WINDOW), 0.25));
    assert_eq!(
        scale.level(rms(-60.0), METER_WINDOW).to_bits(),
        0.0_f32.to_bits()
    );
    assert!(near(scale.level(1.0, METER_WINDOW), 1.0));
    assert!(near(scale.level(rms(-32.0), METER_WINDOW), 0.25));
}

#[test]
fn speech_ceiling_relaxes_six_decibels_a_second_within_its_bounds() {
    let mut scale = SpeechScale::default();
    assert!(near(scale.level(rms(-10.0), METER_WINDOW), 1.0));
    assert_eq!(
        scale.level(rms(-60.0), Duration::from_secs(1)).to_bits(),
        0.0_f32.to_bits()
    );
    assert!(near(scale.level(rms(-35.0), Duration::ZERO), 0.25));
    assert_eq!(
        scale.level(rms(-60.0), Duration::from_secs(60)).to_bits(),
        0.0_f32.to_bits()
    );
    assert!(near(scale.level(rms(-42.0), Duration::ZERO), 0.25));
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

fn record_released(recorder: Recorder<()>) -> Outcome {
    let (events, _reports) = async_channel::bounded(64);
    let (_cues, cues) = async_channel::unbounded();
    let (_keep, keep) = async_channel::bounded(1);
    let (_whole, whole) = async_channel::bounded(1);
    let control = Control::default();
    control.finish();
    recorder
        .record(
            SessionId::FIRST,
            &events,
            &control,
            &Requests { cues, keep, whole },
        )
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

    let mut late = recorder(&spoken(), None)?;
    late.pulse = stalled_since(true)?;
    assert!(
        matches!(record_released(late), Outcome::Finished(Ok(Some(_)))),
        "Samples waiting for a late drain read as a stalled microphone"
    );
    Ok(())
}

#[test]
fn samples_that_arrive_late_restart_the_stall_clock() -> anyhow::Result<()> {
    let mut late = recorder(&spoken(), None)?;
    late.pulse = stalled_since(false)?;
    let (events, reports) = async_channel::bounded(64);
    let (_cues, cues) = async_channel::unbounded();
    let (_keep, keep) = async_channel::bounded(1);
    let (_whole, whole) = async_channel::bounded(1);
    let requests = Requests { cues, keep, whole };
    let control = &Control::default();
    let (ready, recorded) = thread::scope(|scope| {
        let recording = scope.spawn(move || {
            late.record(SessionId::FIRST, &events, control, &requests)
                .outcome
        });
        let ready = std::iter::from_fn(|| reports.recv_blocking().ok())
            .any(|event| matches!(event, CaptureEvent::Ready(_)));
        control.finish();
        (ready, recording.join())
    });
    ensure!(ready, "The recording never reported its first samples");
    let Ok(outcome) = recorded else {
        bail!("The recording thread panicked");
    };
    assert!(
        matches!(outcome, Outcome::Finished(Ok(Some(_)))),
        "The poll after a late first sample read as a microphone that never sent audio"
    );
    Ok(())
}

#[test]
fn stopped_startup_returns_without_audio() {
    let records_nothing = |events: &Sender<CaptureEvent>, control: &Arc<Control>| {
        matches!(
            Recorder::open(None, RecognitionMode::Whole, events, control),
            Err(Startup::Stopped)
        )
    };
    for stop in [Control::finish, Control::cancel] {
        let control = Arc::new(Control::default());
        stop(&control);
        let (events, _audio) = async_channel::bounded(1);
        assert!(records_nothing(&events, &control));
    }
    let (events, _audio) = async_channel::bounded(1);
    events.close();
    assert!(records_nothing(&events, &Arc::default()));
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
        })
    })?;
    let tearing_down = timeout(PATIENCE, teardown.recv()).await;
    let report = timeout(PATIENCE, audio.recv()).await;
    let pending = pending_until_released(capture.retire(), &release).await?;
    tearing_down??;
    assert!(
        matches!(report?, Ok(CaptureEvent::Finished(SessionId::FIRST, Ok(Some(sealed)))) if sealed.wav == [1, 2])
    );
    assert!(pending, "Retirement acknowledged before device teardown");
    Ok(())
}

#[tokio::test]
async fn capture_retirement_signals_cancel_and_waits_for_teardown() -> anyhow::Result<()> {
    let (events, _audio) = async_channel::bounded(1);
    let (started, starting) = async_channel::bounded(1);
    let (release, waiting) = async_channel::bounded(1);
    let capture = Capture::spawn(SessionId::FIRST, events, move |_, control, _| {
        started.send_blocking(Arc::clone(control))?;
        waiting.recv_blocking()?;
        Ok(Captured {
            outcome: Outcome::Finished(Ok(None)),
            device: (),
        })
    })?;
    let control = starting.recv().await?;
    let retirement = capture.retire();
    let mode = control.mode();
    let pending = pending_until_released(retirement, &release).await?;
    assert_eq!(mode, Mode::Cancelled);
    assert!(pending, "Retirement acknowledged before teardown");
    Ok(())
}

#[tokio::test]
async fn finish_and_retirement_wake_the_owned_consumer_even_before_it_parks() -> anyhow::Result<()>
{
    let mut missed = Vec::new();
    for (cancel, wake_before_park) in [(false, false), (false, true), (true, false), (true, true)] {
        if !consumer_wakes(cancel, wake_before_park).await? {
            missed.push((cancel, wake_before_park));
        }
    }
    assert!(
        missed.is_empty(),
        "Capture wake missed (cancel, before park): {missed:?}"
    );
    Ok(())
}

fn poisoned<T>(error: PoisonError<T>) -> anyhow::Error {
    anyhow!("Capture fixture gate was poisoned: {error}")
}

async fn consumer_wakes(cancel: bool, wake_before_park: bool) -> anyhow::Result<bool> {
    let (events, audio) = async_channel::bounded(1);
    let (armed, arming) = async_channel::bounded(1);
    #[expect(
        clippy::disallowed_types,
        reason = "A test-only condition variable forces native-thread park ordering without microphone access"
    )]
    let gate = Arc::new((
        std::sync::Mutex::new(!wake_before_park),
        std::sync::Condvar::new(),
    ));
    let waiting = Arc::clone(&gate);
    let capture = Capture::spawn(SessionId::FIRST, events, move |_, control, _| {
        ensure!(control.mode() == Mode::Recording);
        armed.send_blocking(thread::current())?;
        let (lock, ready) = &*waiting;
        let released = lock.lock().map_err(poisoned)?;
        drop(
            ready
                .wait_while(released, |released| !*released)
                .map_err(poisoned)?,
        );
        thread::park_timeout(Duration::from_secs(5));
        while control.mode() == Mode::Recording {
            thread::park_timeout(Duration::from_secs(5));
        }
        Ok(Captured {
            outcome: Outcome::Finished(Ok(None)),
            device: (),
        })
    })?;
    let consumer = timeout(PATIENCE, arming.recv()).await??;
    let release = || -> anyhow::Result<()> {
        let (lock, ready) = &*gate;
        *lock.lock().map_err(poisoned)? = true;
        ready.notify_one();
        Ok(())
    };
    if cancel {
        let retirement = capture.retire();
        tokio::pin!(retirement);
        release()?;
        let woke = timeout(Duration::from_millis(400), retirement.as_mut())
            .await
            .is_ok();
        consumer.unpark();
        if !woke {
            timeout(PATIENCE, retirement).await?;
        }
        Ok(woke)
    } else {
        capture.finish();
        let mode = capture.signal.control.mode();
        release()?;
        let completed = timeout(Duration::from_millis(400), audio.recv()).await;
        consumer.unpark();
        timeout(PATIENCE, capture.retire()).await?;
        assert_eq!(mode, Mode::Finishing);
        Ok(matches!(
            completed,
            Ok(Ok(CaptureEvent::Finished(SessionId::FIRST, Ok(None))))
        ))
    }
}

#[tokio::test]
async fn closing_audio_events_unblocks_owned_capture_retirement() -> anyhow::Result<()> {
    let (events, audio) = async_channel::bounded(1);
    events.try_send(CaptureEvent::Ready(SessionId::FIRST))?;
    let capture = Capture::spawn(SessionId::FIRST, events, |_, _, _| {
        Ok(Captured {
            outcome: Outcome::Finished(Ok(None)),
            device: (),
        })
    })?;
    let retirement = capture.retire();
    tokio::pin!(retirement);
    assert!(still_pending(retirement.as_mut()).await);
    audio.close();
    timeout(PATIENCE, retirement).await?;
    Ok(())
}

#[tokio::test]
async fn a_panicked_capture_reports_failure_and_still_retires() -> anyhow::Result<()> {
    let (events, audio) = async_channel::bounded(1);
    let capture = Capture::spawn(
        SessionId::FIRST,
        events,
        |_, _, _| -> anyhow::Result<Captured<()>> {
            panic::resume_unwind(Box::new(()));
        },
    )?;
    let report = timeout(PATIENCE, audio.recv()).await??;
    assert!(
        matches!(&report, CaptureEvent::Finished(SessionId::FIRST, Err(error))
            if error.to_string() == CAPTURE_STOPPED),
        "A panicked capture left its session waiting"
    );
    timeout(PATIENCE, capture.retire()).await?;
    Ok(())
}

#[tokio::test]
async fn a_finished_capture_refuses_a_late_show_checkpoint() -> anyhow::Result<()> {
    struct Device(Receiver<()>);
    impl Drop for Device {
        fn drop(&mut self) {
            self.0.recv_blocking().unwrap_or_default();
        }
    }
    let root = tempfile::tempdir()?;
    let parent = root.path().to_owned();
    let (events, audio) = async_channel::bounded(1);
    let (release, waiting) = async_channel::bounded(1);
    let capture = Capture::spawn(SessionId::FIRST, events, move |_, _, _| {
        Ok(Captured {
            outcome: Outcome::Finished(Ok(None)),
            device: Device(waiting),
        })
    })?;
    let report = timeout(PATIENCE, audio.recv()).await??;
    let Kept {
        keeper,
        frames,
        audio: log,
    } = Keeper::start(move || Ok(parent), SystemTime::now())?;
    drop(frames);
    let accepted = capture.keep(log, Duration::ZERO, "en");
    release.try_send(())?;
    timeout(PATIENCE, capture.retire()).await?;
    keeper.close().await?;
    assert!(matches!(
        report,
        CaptureEvent::Finished(SessionId::FIRST, Ok(None))
    ));
    assert!(
        !accepted,
        "A finished capture accepted Show audio it can never checkpoint"
    );
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
    let loud_show_cue: Vec<u8> = sounds::pcm(Cue::Show)
        .as_chunks::<SAMPLE_BYTES>()
        .0
        .iter()
        .flat_map(|pair| i16::from_le_bytes(*pair).saturating_mul(3).to_le_bytes())
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
