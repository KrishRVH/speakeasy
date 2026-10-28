//! Checks the capture thread's lifecycle, retirement and teardown, its metering, PCM consumption,
//! and the cue notices its speech gate hears, with fake devices and synthetic samples; no test opens
//! a microphone.

use std::sync::PoisonError;

use anyhow::ensure;
use tokio::time::timeout;

use super::{pcm::SAMPLE_BYTES, *};
use crate::sounds::Cue;

const PATIENCE: Duration = Duration::from_secs(2);

#[test]
fn speech_meter_spans_sixty_to_six_dbfs() {
    let at_dbfs = |dbfs: f32| speech_meter_level(10.0_f32.powf(dbfs / 20.0));
    assert!(at_dbfs(-60.0).abs() < 1e-5);
    assert!((at_dbfs(-33.0) - 0.5).abs() < 1e-5);
    assert!((at_dbfs(-6.0) - 1.0).abs() < 1e-5);
    assert_eq!(speech_meter_level(0.0).to_bits(), 0.0_f32.to_bits());
    assert_eq!(speech_meter_level(1.0).to_bits(), 1.0_f32.to_bits());
}

#[test]
fn stopped_startup_returns_without_audio() -> anyhow::Result<()> {
    let records_nothing =
        |events: &Sender<CaptureEvent>, control: &Arc<Control>| -> anyhow::Result<bool> {
            let no_cues = async_channel::unbounded().1;
            Ok(record(SessionId::FIRST, None, events, control, &no_cues)?
                .outcome?
                .is_none())
        };
    for stop in [Control::finish, Control::cancel] {
        let control = Arc::new(Control::default());
        stop(&control);
        let (events, _audio) = async_channel::bounded(1);
        assert!(records_nothing(&events, &control)?);
    }
    let (events, _audio) = async_channel::bounded(1);
    events.close();
    assert!(records_nothing(&events, &Arc::default())?);
    Ok(())
}

/// Whether `retirement` stays pending until `release` lets the blocked capture end. It answers only
/// once the capture is released and retired, so a failing assertion cannot unwind into a join on the
/// blocked thread.
async fn pending_until_released(
    retirement: impl Future<Output = ()>,
    release: &Sender<()>,
) -> anyhow::Result<bool> {
    tokio::pin!(retirement);
    let pending = timeout(Duration::from_millis(30), retirement.as_mut())
        .await
        .is_err();
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
            // Closing the fixture gate on failure also releases teardown.
            self.release.recv_blocking().unwrap_or_default();
        }
    }
    let (events, audio) = async_channel::bounded(1);
    let (entered, teardown) = async_channel::bounded(1);
    let (release, waiting) = async_channel::bounded(1);
    let capture = Capture::spawn(SessionId::FIRST, events, move |_, _, _| {
        Ok(Captured {
            outcome: Ok(Some(Sealed {
                wav: vec![1, 2],
                trimmed: Duration::ZERO,
            })),
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
            outcome: Ok(None),
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

#[expect(
    clippy::disallowed_types,
    reason = "A test-only condition variable forces native-thread park ordering without microphone access"
)]
async fn consumer_wakes(cancel: bool, wake_before_park: bool) -> anyhow::Result<bool> {
    use std::sync::{Condvar, Mutex};

    let (events, audio) = async_channel::bounded(1);
    let (armed, arming) = async_channel::bounded(1);
    let gate = Arc::new((Mutex::new(!wake_before_park), Condvar::new()));
    let waiting = Arc::clone(&gate);
    let capture = Capture::spawn(SessionId::FIRST, events, move |_, control, _| {
        // Arm after reading the control, so a lost notification cannot pass by finishing before
        // the thread enters its wait.
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
            outcome: Ok(None),
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
        // A fallback wake reaps the thread even when the notification under test is lost.
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
            outcome: Ok(None),
            device: (),
        })
    })?;
    let retirement = capture.retire();
    tokio::pin!(retirement);
    assert!(
        timeout(Duration::from_millis(30), retirement.as_mut())
            .await
            .is_err()
    );
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
            if error.to_string().contains("stopped unexpectedly")),
        "A panicked capture left its session waiting"
    );
    timeout(PATIENCE, capture.retire()).await?;
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
    consume_pcm(&mut ring, &mut pcm, &mut meter);
    assert_eq!(meter.samples, 3);
    assert_eq!(meter.energy, 1.25);
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
    let loud_show_cue: Vec<u8> = Cue::Show
        .pcm()
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
        classify(&mut speech, &loud_show_cue, &notices);
        speech.retained(loud_show_cue.len())
    };
    assert!(
        retained(false).is_some(),
        "An unnoticed loud cue must count as speech, or this test proves nothing"
    );
    assert_eq!(retained(true), None);
}
