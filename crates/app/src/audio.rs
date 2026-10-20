//! Microphone capture on an owned thread. The real-time callback queues mono samples without
//! allocating; the capture thread encodes them in place behind a reserved WAV header.

mod control;
mod pcm;
mod speech;

use std::{
    convert::Infallible,
    num::NonZeroUsize,
    panic::{self, AssertUnwindSafe},
    sync::Arc,
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail, ensure};
use async_channel::{Receiver, Sender};
use cpal::{
    SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};
use speakeasy_core::gesture::RECORDING_LIMIT;

pub(crate) use self::pcm::{Sealed, silent_wav, wav_sample_rate};
use self::{
    control::{Control, Mode},
    pcm::{MAX_SAMPLE_RATE, Pcm16, samples_in},
    speech::Speech,
};
use crate::{
    ports::Recording,
    runtime::{CaptureEvent, SessionId},
};

const MAX_CHANNELS: usize = 32;
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
        work: impl FnOnce(&Sender<CaptureEvent>, &Arc<Control>) -> anyhow::Result<Recorded<D>>
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
                    Ok(Recorded { outcome, device }) => (outcome, Some(device)),
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
struct Recorded<D> {
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
    speech: Speech,
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
        let speech = Speech::new(rate).context("Unsupported microphone format")?;
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
    ) -> Recorded<LiveStream> {
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
        Recorded { outcome, device }
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

/// Everything the stream callbacks own for one capture.
struct CallbackState {
    channels: NonZeroUsize,
    producer: Producer<f32>,
    control: Arc<Control>,
    errors: Sender<cpal::Error>,
    started: Instant,
    limit: usize,
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
) -> anyhow::Result<Recorded<Option<LiveStream>>> {
    match Recorder::open(microphone, events, control) {
        Ok(recorder) => {
            let Recorded { outcome, device } = recorder.record(id, events, control);
            Ok(Recorded {
                outcome,
                device: Some(device),
            })
        },
        Err(Startup::Stopped) => Ok(Recorded {
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

fn rate_and_channels(format: &cpal::SupportedStreamConfig) -> anyhow::Result<(u32, NonZeroUsize)> {
    let rate = format.sample_rate();
    let channels = NonZeroUsize::new(usize::from(format.channels()))
        .context("Unsupported microphone format: no input channels")?;
    ensure!(
        (1..=MAX_SAMPLE_RATE).contains(&rate) && channels.get() <= MAX_CHANNELS,
        "Unsupported microphone format"
    );
    Ok((rate, channels))
}

fn build_stream(
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

fn capture_failure(control: &Control, errors: &Receiver<cpal::Error>) -> anyhow::Result<()> {
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

fn microphone_error(error: cpal::Error) -> anyhow::Error {
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

    use tokio::time::timeout;

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(2);

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
            Ok(Recorded {
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

    #[tokio::test]
    async fn capture_retirement_signals_cancel_and_waits_for_teardown() -> anyhow::Result<()> {
        let (events, _audio) = async_channel::bounded(1);
        let (started, starting) = async_channel::bounded(1);
        let (release, waiting) = async_channel::bounded(1);
        let capture = Capture::spawn(SessionId::FIRST, events, move |_, control| {
            started.send_blocking(control.clone())?;
            waiting.recv_blocking()?;
            Ok(Recorded {
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
            Ok(Recorded {
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
            Ok(Recorded {
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
            |_, _| -> anyhow::Result<Recorded<()>> {
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

    #[test]
    #[ignore = "opt-in synthetic release profiling; never opens a microphone"]
    fn profile_audio_pipeline() -> anyhow::Result<()> {
        use std::hint::black_box;
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
}
