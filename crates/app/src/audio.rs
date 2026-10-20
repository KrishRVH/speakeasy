//! Microphone capture on an owned thread, from device selection through draining and metering to
//! sealing audio before driver teardown. The real-time callbacks live in `stream`, the in-place
//! WAV buffer in `pcm`.

mod control;
mod pcm;
mod speech;
mod stream;

use std::{
    convert::Infallible,
    panic::{self, AssertUnwindSafe},
    sync::Arc,
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow};
use async_channel::{Receiver, Sender};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, RingBuffer};
use speakeasy_core::gesture::RECORDING_LIMIT;

pub(crate) use self::pcm::{Sealed, silent_wav, wav_sample_rate};
use self::{
    control::{Control, Mode},
    pcm::{Pcm16, samples_in},
    speech::Audibility,
    stream::{CallbackState, build_stream, capture_failure, microphone_error, rate_and_channels},
};
use crate::{
    ports::Recording,
    runtime::{CaptureEvent, SessionId},
};

const INITIAL_RESERVATION: Duration = Duration::from_secs(10);
const METER_INTERVAL: Duration = Duration::from_millis(32);
const DRAIN_INTERVAL: Duration = Duration::from_millis(5);
const METER_FLOOR_DBFS: f32 = -60.0;
const METER_TOP_DBFS: f32 = -6.0;

/// One microphone capture on its own thread. Dropping it cancels without joining, so it cannot be
/// an `OwnedThread`; the session owner retires it to await native teardown.
pub(crate) struct Capture {
    signal: Signal,
    /// Never sends; it closes once the thread has torn down its stream and reported `Finished`.
    exit: Sender<Infallible>,
    thread: JoinHandle<()>,
}

impl Capture {
    pub(crate) fn start(
        id: SessionId,
        microphone: Option<&str>,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Self> {
        let microphone = microphone.map(str::to_owned);
        Self::spawn(id, events, move |events, control| {
            record(id, microphone.as_deref(), events, control)
        })
    }

    fn spawn<D: 'static>(
        id: SessionId,
        events: Sender<CaptureEvent>,
        work: impl FnOnce(&Sender<CaptureEvent>, &Arc<Control>) -> anyhow::Result<Captured<D>>
        + Send
        + 'static,
    ) -> anyhow::Result<Self> {
        let control = Arc::new(Control::default());
        let (exit, alive) = async_channel::bounded::<Infallible>(1);
        let thread = thread::Builder::new().name("audio-capture".into()).spawn({
            let control = control.clone();
            move || {
                // A panicked capture's state is discarded; only its event lane is used afterwards.
                let result = panic::catch_unwind(AssertUnwindSafe(|| work(&events, &control)))
                    .unwrap_or_else(|_| {
                        Err(anyhow!("Microphone stopped unexpectedly. Try recording again."))
                    });
                let (outcome, device) = match result {
                    Ok(Captured { outcome, device }) => (outcome, Some(device)),
                    Err(error) => (Err(error), None),
                };
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A closed event lane belongs to a departed owner; retirement still observes this thread's exit"
                )]
                let _ = events.send_blocking(CaptureEvent::Finished(id, outcome));
                // Audio is sealed before this potentially blocking driver teardown. Retirement
                // acknowledges only after teardown, so another capture cannot overlap it.
                drop(device);
                // Naming `alive` moves it into this closure; dropping it last closes `exit`.
                drop(alive);
            }
        })?;
        let signal = Signal {
            control,
            thread: thread.thread().clone(),
        };
        Ok(Self {
            signal,
            exit,
            thread,
        })
    }
}

impl Recording for Capture {
    fn finish(&self) {
        self.signal.finish();
    }

    fn retire(self) -> impl Future<Output = ()> + Send + 'static {
        let Self {
            signal,
            exit,
            thread,
        } = self;
        // Dropping the signal cancels capture and wakes the thread.
        drop(signal);
        async move {
            exit.closed().await;
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The capture already reported its outcome; a join error leaves no session to fail"
            )]
            let _ = thread.join();
        }
    }
}

/// Finish and cancel requests for the capture thread. Dropping it cancels without joining, so
/// discarding a capture never waits for an audio driver on the owner's input path.
struct Signal {
    control: Arc<Control>,
    thread: Thread,
}

impl Signal {
    fn finish(&self) {
        self.control.finish();
        self.thread.unpark();
    }
}

impl Drop for Signal {
    fn drop(&mut self) {
        self.control.cancel();
        self.thread.unpark();
    }
}

/// Why a capture ended before its stream played.
enum Startup {
    Stopped,
    Failed(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for Startup {
    fn from(error: E) -> Self {
        Self::Failed(error.into())
    }
}

/// The sealed outcome and resources retained until it has been reported. The generic device is
/// an internal seam for testing blocked teardown without opening a microphone.
struct Captured<D> {
    outcome: anyhow::Result<Option<Sealed>>,
    device: D,
}

/// Fields drop in declaration order, so the stream stops before its device and host.
struct LiveStream {
    _stream: cpal::Stream,
    _device: cpal::Device,
    _host: cpal::Host,
}

/// A playing stream and the audio it has delivered so far. Unsealed PCM is erased on drop.
struct Recorder {
    pcm: Pcm16,
    speech: Audibility,
    device: LiveStream,
    ring: Consumer<f32>,
    errors: Receiver<cpal::Error>,
    started: Instant,
    rate: u32,
    limit: usize,
}

impl Recorder {
    fn open(
        microphone: Option<&str>,
        events: &Sender<CaptureEvent>,
        control: &Arc<Control>,
    ) -> Result<Self, Startup> {
        // Driver calls can block; recheck between them so a cancelled startup never plays a stream.
        let checkpoint = || {
            if control.mode() == Mode::Recording && !events.is_closed() {
                Ok(())
            } else {
                Err(Startup::Stopped)
            }
        };
        checkpoint()?;
        let host = cpal::default_host();
        checkpoint()?;
        let device = select_device(&host, microphone, checkpoint)?;
        checkpoint()?;
        let format = device.default_input_config().map_err(microphone_error)?;
        checkpoint()?;
        let (rate, channels) = rate_and_channels(&format)?;
        let sample_rate = usize::try_from(rate)?;
        let (producer, ring) = RingBuffer::new(sample_rate);
        let (errors, stream_errors) = async_channel::bounded(1);
        let started = Instant::now();
        let limit = sample_rate
            .checked_mul(usize::try_from(RECORDING_LIMIT.as_secs())?)
            .context("Microphone recording limit is unsupported")?;
        let callbacks = CallbackState {
            channels,
            producer,
            control: control.clone(),
            errors,
            started,
            limit,
        };
        let stream = build_stream(&device, &format, callbacks)?;
        checkpoint()?;
        // PCM stays at the device's native rate; the engine resamples.
        let pcm = Pcm16::with_reservation(samples_in(INITIAL_RESERVATION, rate));
        let speech = Audibility::new(rate).context("Unsupported microphone format")?;
        checkpoint()?;
        stream.play().map_err(microphone_error)?;
        Ok(Self {
            pcm,
            speech,
            device: LiveStream {
                _stream: stream,
                _device: device,
                _host: host,
            },
            ring,
            errors: stream_errors,
            started,
            rate,
            limit,
        })
    }

    fn record(
        mut self,
        id: SessionId,
        events: &Sender<CaptureEvent>,
        control: &Control,
    ) -> Captured<LiveStream> {
        let drained = self.drain(id, events, control);
        control.finish();
        control.quiesce();
        let Self {
            pcm,
            speech,
            device,
            errors,
            rate,
            ..
        } = self;
        let outcome = drained.and_then(|finished| {
            if !finished || control.mode() == Mode::Cancelled {
                return Ok(None);
            }
            // A callback may have failed between the last poll and quiescence.
            capture_failure(control, &errors)?;
            pcm.seal(speech, rate)
        });
        Captured { outcome, device }
    }

    fn drain(
        &mut self,
        id: SessionId,
        events: &Sender<CaptureEvent>,
        control: &Control,
    ) -> anyhow::Result<bool> {
        let mut meter = LevelMeter::new();
        let mut ready = false;
        loop {
            if control.mode() == Mode::Cancelled || events.is_closed() {
                return Ok(false);
            }
            capture_failure(control, &self.errors)?;
            let stopping = control.mode() == Mode::Finishing
                || self.started.elapsed() >= RECORDING_LIMIT
                || self.pcm.samples() >= self.limit;
            if stopping {
                control.finish();
                control.quiesce();
            }
            if !ready && !self.ring.is_empty() {
                ready = true;
                events.send_blocking(CaptureEvent::Ready(id))?;
            }
            consume_pcm(&mut self.ring, &mut self.pcm, &mut meter, self.limit);
            self.speech.extend(self.pcm.audio());
            if let Some(level) = meter.take_level() {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Meter updates are expendable presentation; a full or closed event lane must not interrupt audio capture"
                )]
                let _ = events.try_send(CaptureEvent::Level(id, level));
            }
            if stopping {
                return Ok(true);
            }
            // Finish and cancel unpark this thread at once, and an unpark that lands before the
            // wait is not lost; the timeout only paces draining of the ring.
            thread::park_timeout(DRAIN_INTERVAL);
        }
    }
}

/// Accumulates sample energy and reports a speech level once per meter interval.
struct LevelMeter {
    energy: f64,
    samples: u32,
    since: Instant,
}

impl LevelMeter {
    fn new() -> Self {
        Self {
            energy: 0.0,
            samples: 0,
            since: Instant::now(),
        }
    }

    fn add(&mut self, sample: f32) {
        self.energy += f64::from(sample * sample);
        self.samples = self.samples.saturating_add(1);
    }

    fn take_level(&mut self) -> Option<f32> {
        if self.since.elapsed() < METER_INTERVAL {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "The display meter intentionally converts bounded normalized RMS to f32; PCM remains independently encoded"
        )]
        let rms = (self.energy / f64::from(self.samples.max(1))).sqrt() as f32;
        *self = Self::new();
        Some(speech_meter_level(rms))
    }
}

pub(crate) struct InputDevice {
    /// The stable device identifier saved in settings.
    pub id: String,
    /// For display only; device names need not be unique.
    pub name: String,
}

fn record(
    id: SessionId,
    microphone: Option<&str>,
    events: &Sender<CaptureEvent>,
    control: &Arc<Control>,
) -> anyhow::Result<Captured<Option<LiveStream>>> {
    match Recorder::open(microphone, events, control) {
        Ok(recorder) => {
            let Captured { outcome, device } = recorder.record(id, events, control);
            Ok(Captured {
                outcome,
                device: Some(device),
            })
        },
        Err(Startup::Stopped) => Ok(Captured {
            outcome: Ok(None),
            device: None,
        }),
        Err(Startup::Failed(error)) => Err(error),
    }
}

fn select_device(
    host: &cpal::Host,
    microphone: Option<&str>,
    checkpoint: impl Fn() -> Result<(), Startup>,
) -> Result<cpal::Device, Startup> {
    let Some(id) = microphone else {
        return Ok(host
            .default_input_device()
            .context("No microphone found. Connect a microphone and try again.")?);
    };
    for device in host.input_devices()? {
        checkpoint()?;
        if device.id().is_ok_and(|actual| actual.to_string() == id) {
            return Ok(device);
        }
    }
    checkpoint()?;
    Err(
        anyhow!("Selected microphone is disconnected. Reconnect it or choose another in Settings.")
            .into(),
    )
}

/// Drains the ring, appending samples to `pcm` until it holds `limit` and metering each one kept.
fn consume_pcm(ring: &mut Consumer<f32>, pcm: &mut Pcm16, meter: &mut LevelMeter, limit: usize) {
    let Ok(chunk) = ring.read_chunk(ring.slots()) else {
        return;
    };
    let (first, second) = chunk.as_slices();
    let room = limit.saturating_sub(pcm.samples());
    for &sample in first.iter().chain(second).take(room) {
        let sample = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        meter.add(sample);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "A finite sample clamped to [-1, 1] intentionally quantizes to signed PCM16 without overflow"
        )]
        let encoded = (sample * 32_767.0) as i16;
        pcm.push(encoded, limit);
    }
    chunk.commit_all();
}

/// Maps RMS onto the meter's dBFS span, whose top is a speech reference rather than clipping.
fn speech_meter_level(rms: f32) -> f32 {
    let above_floor = 20.0_f32.mul_add(rms.max(0.000_001).log10(), -METER_FLOOR_DBFS);
    (above_floor / (METER_TOP_DBFS - METER_FLOOR_DBFS)).clamp(0.0, 1.0)
}

/// Lists input devices without opening a stream. Drivers may block, and CPAL leaves its calling
/// thread in a single-threaded COM apartment, so enumeration runs on its own detached thread.
pub(crate) async fn microphones() -> anyhow::Result<Vec<InputDevice>> {
    let (reply, devices) = async_channel::bounded(1);
    thread::Builder::new()
        .name("microphone-scan".into())
        .spawn(move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Closing the one-shot receiver means the settings owner no longer needs the device list"
            )]
            let _ = reply.send_blocking(enumerate_microphones());
        })?;
    devices.recv().await?
}

fn enumerate_microphones() -> anyhow::Result<Vec<InputDevice>> {
    cpal::default_host()
        .input_devices()?
        .map(|device| {
            Ok(InputDevice {
                id: device.id()?.to_string(),
                name: device.description()?.name().to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::PoisonError;

    use anyhow::ensure;
    use tokio::time::timeout;

    use super::*;

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
        for mode in [Mode::Finishing, Mode::Cancelled] {
            let control = Arc::new(Control::default());
            if mode == Mode::Finishing {
                control.finish();
            } else {
                control.cancel();
            }
            let (events, _audio) = async_channel::bounded(1);
            assert!(
                record(SessionId::FIRST, None, &events, &control)?
                    .outcome?
                    .is_none()
            );
        }
        let (events, _audio) = async_channel::bounded(1);
        events.close();
        let control = Arc::new(Control::default());
        assert!(
            record(SessionId::FIRST, None, &events, &control)?
                .outcome?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn sealed_audio_is_reported_before_device_teardown_and_retirement_waits()
    -> anyhow::Result<()> {
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
        let capture = Capture::spawn(SessionId::FIRST, events, move |_, _| {
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
        timeout(PATIENCE, teardown.recv()).await??;
        let report = timeout(PATIENCE, audio.recv()).await;
        let retirement = capture.retire();
        tokio::pin!(retirement);
        let pending = timeout(Duration::from_millis(30), retirement.as_mut())
            .await
            .is_err();
        release.send(()).await?;
        timeout(PATIENCE, retirement).await?;
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
        let capture = Capture::spawn(SessionId::FIRST, events, move |_, control| {
            started.send_blocking(control.clone())?;
            waiting.recv_blocking()?;
            Ok(Captured {
                outcome: Ok(None),
                device: (),
            })
        })?;
        let control = starting.recv().await?;
        let retirement = capture.retire();
        assert_eq!(control.mode(), Mode::Cancelled);
        tokio::pin!(retirement);
        assert!(
            timeout(Duration::from_millis(30), retirement.as_mut())
                .await
                .is_err()
        );
        release.send(()).await?;
        timeout(PATIENCE, retirement).await?;
        Ok(())
    }

    #[tokio::test]
    async fn finish_and_retirement_wake_the_owned_consumer_even_before_it_parks()
    -> anyhow::Result<()> {
        let mut missed = Vec::new();
        for (cancel, wake_before_park) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
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
        let waiting = gate.clone();
        let capture = Capture::spawn(SessionId::FIRST, events, move |_, control| {
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
            let completed = timeout(Duration::from_millis(400), retirement.as_mut()).await;
            // A fallback wake reaps the thread even when the notification under test is lost.
            consumer.unpark();
            if completed.is_err() {
                timeout(PATIENCE, retirement).await?;
                return Ok(false);
            }
        } else {
            capture.finish();
            assert_eq!(capture.signal.control.mode(), Mode::Finishing);
            release()?;
            let completed = timeout(Duration::from_millis(400), audio.recv()).await;
            consumer.unpark();
            timeout(PATIENCE, capture.retire()).await?;
            if !matches!(
                completed,
                Ok(Ok(CaptureEvent::Finished(SessionId::FIRST, Ok(None))))
            ) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[tokio::test]
    async fn closing_audio_events_unblocks_owned_capture_retirement() -> anyhow::Result<()> {
        let (events, audio) = async_channel::bounded(1);
        events.try_send(CaptureEvent::Ready(SessionId::FIRST))?;
        let capture = Capture::spawn(SessionId::FIRST, events, |_, _| {
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
            |_, _| -> anyhow::Result<Captured<()>> {
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
        let mut pcm = Pcm16::with_reservation(0);
        let mut meter = LevelMeter::new();
        consume_pcm(&mut ring, &mut pcm, &mut meter, 3);
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
}
