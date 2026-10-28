//! Microphone capture on an owned thread, from device selection through draining and metering to
//! sealing audio before driver teardown. The real-time callbacks live in `stream`, the in-place WAV
//! buffer in `pcm`, the speech gate, which hears a voice beneath Speakeasy's own cues, in `speech`,
//! and the input devices Settings offers in `microphones`.

mod control;
mod microphones;
mod pcm;
mod speech;
mod stream;

use std::{
    panic::{self, AssertUnwindSafe},
    sync::Arc,
    thread::{self, Thread},
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow};
use async_channel::{Receiver, Sender};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, RingBuffer};
use speakeasy_core::gesture::RECORDING_LIMIT;
use speakeasy_platform::OwnedThread;

use self::{
    control::{Control, Mode},
    pcm::{Pcm16, samples_in},
    speech::Audibility,
    stream::{CallbackState, build_stream, capture_failure, microphone_error, rate_and_channels},
};
pub(crate) use self::{
    microphones::{InputDevice, MicrophoneScan},
    pcm::{INCOMPLETE_HEADER, silent_wav, wav_sample_rate},
};
use crate::runtime::ports::{CaptureEvent, Capturing, Listening, Sealed, SessionId};

const INITIAL_RESERVATION: Duration = Duration::from_secs(10);
/// Audio the callbacks can queue ahead of the drain loop before capture overflows.
const RING_CAPACITY: Duration = Duration::from_secs(1);
const METER_INTERVAL: Duration = Duration::from_millis(32);
const DRAIN_INTERVAL: Duration = Duration::from_millis(5);
const METER_FLOOR_DBFS: f32 = -60.0;
const METER_TOP_DBFS: f32 = -6.0;
const CAPTURE_STOPPED: &str = "Microphone stopped unexpectedly. Try recording again.";

/// One microphone capture on its own thread. Fields drop in declaration order, so a capture
/// dropped without retiring cancels before its thread joins; the session owner retires it instead,
/// awaiting native teardown without blocking its executor.
pub(crate) struct Capture {
    signal: Signal,
    /// Each message says a cue is starting, so the speech gate listens beneath it.
    cues: Sender<()>,
    /// Exits once it has reported `Finished` and torn down its stream.
    thread: OwnedThread,
}

impl Capture {
    pub(crate) fn start(
        id: SessionId,
        microphone: Option<&str>,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Self> {
        let microphone = microphone.map(str::to_owned);
        Self::spawn(id, events, move |events, control, cues| {
            record(id, microphone.as_deref(), events, control, cues)
        })
    }

    fn spawn<D: 'static>(
        id: SessionId,
        events: Sender<CaptureEvent>,
        work: impl FnOnce(
            &Sender<CaptureEvent>,
            &Arc<Control>,
            &Receiver<()>,
        ) -> anyhow::Result<Captured<D>>
        + Send
        + 'static,
    ) -> anyhow::Result<Self> {
        let control = Arc::new(Control::default());
        let (cues, heard) = async_channel::unbounded();
        let thread = OwnedThread::spawn("audio-capture", {
            let control = Arc::clone(&control);
            move || {
                // A panicked capture's state is discarded; only its event lane is used afterwards.
                let ran = panic::catch_unwind(AssertUnwindSafe(|| work(&events, &control, &heard)));
                let (outcome, device) = match ran {
                    Ok(Ok(Captured { outcome, device })) => (outcome, Some(device)),
                    Ok(Err(error)) => (Err(error), None),
                    Err(_) => (Err(anyhow!(CAPTURE_STOPPED)), None),
                };
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A closed event lane belongs to a departed owner; retirement still observes this thread's exit"
                )]
                let _ = events.send_blocking(CaptureEvent::Finished(id, outcome));
                // Audio is sealed before this potentially blocking driver teardown. Retirement
                // acknowledges only after teardown, so another capture cannot overlap it.
                drop(device);
            }
        })?;
        let signal = Signal {
            control,
            thread: thread.thread().clone(),
        };
        Ok(Self {
            signal,
            cues,
            thread,
        })
    }
}

impl Capturing for Capture {
    fn finish(&self) {
        self.signal.finish();
    }

    fn retire(self) -> impl Future<Output = ()> + Send + 'static {
        let Self { signal, thread, .. } = self;
        drop(signal);
        thread.join()
    }
}

impl Listening for Capture {
    fn expect_cue(&self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A capture that has ended has no speech left to gate"
        )]
        let _ = self.cues.try_send(());
    }
}

/// Finish and cancel requests for the capture thread. Dropping it cancels without waiting, so
/// retiring a capture never waits for an audio driver on the owner's input path.
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
        let (producer, ring) = RingBuffer::new(samples_in(RING_CAPACITY, rate));
        let (errors, stream_errors) = async_channel::bounded(1);
        let started = Instant::now();
        let limit = samples_in(RECORDING_LIMIT, rate);
        let callbacks = CallbackState {
            channels,
            producer,
            control: Arc::clone(control),
            errors,
            started,
            limit,
        };
        let stream = build_stream(&device, &format, callbacks)?;
        // PCM stays at the device's native rate; the engine resamples.
        let pcm = Pcm16::with_reservation(samples_in(INITIAL_RESERVATION, rate), limit);
        let speech = Audibility::new(rate);
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
        })
    }

    fn record(
        mut self,
        id: SessionId,
        events: &Sender<CaptureEvent>,
        control: &Control,
        cues: &Receiver<()>,
    ) -> Captured<LiveStream> {
        let drained = self.drain(id, events, control, cues);
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
        let outcome = drained.and_then(|()| {
            if abandoned(control, events) {
                return Ok(None);
            }
            // A callback may have failed between the last poll and quiescence.
            capture_failure(control, &errors)?;
            pcm.seal(speech, rate)
        });
        Captured { outcome, device }
    }

    /// Moves audio from the ring into PCM, reporting readiness and levels, until capture finishes,
    /// reaches the limit, or is abandoned.
    fn drain(
        &mut self,
        id: SessionId,
        events: &Sender<CaptureEvent>,
        control: &Control,
        cues: &Receiver<()>,
    ) -> anyhow::Result<()> {
        let mut meter = LevelMeter::new();
        let mut ready = false;
        loop {
            if abandoned(control, events) {
                return Ok(());
            }
            capture_failure(control, &self.errors)?;
            let stopping = control.mode() == Mode::Finishing
                || self.started.elapsed() >= RECORDING_LIMIT
                || self.pcm.room() == 0;
            if stopping {
                control.finish();
                control.quiesce();
            }
            consume_pcm(&mut self.ring, &mut self.pcm, &mut meter);
            // Checked after draining, not before, so a callback that fills the ring in between
            // cannot leave audio in the recording that no `Ready` announced.
            if !ready && self.pcm.samples() > 0 {
                ready = true;
                events.send_blocking(CaptureEvent::Ready(id))?;
            }
            classify(&mut self.speech, self.pcm.audio(), cues);
            if let Some(level) = meter.take_level() {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Meter updates are expendable presentation; a full or closed event lane must not interrupt audio capture"
                )]
                let _ = events.try_send(CaptureEvent::Level(id, level));
            }
            if stopping {
                return Ok(());
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

fn record(
    id: SessionId,
    microphone: Option<&str>,
    events: &Sender<CaptureEvent>,
    control: &Arc<Control>,
    cues: &Receiver<()>,
) -> anyhow::Result<Captured<Option<LiveStream>>> {
    match Recorder::open(microphone, events, control) {
        Ok(recorder) => {
            let Captured { outcome, device } = recorder.record(id, events, control, cues);
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
    for device in host.input_devices().map_err(microphone_error)? {
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

/// Drains the ring, appending samples to `pcm` until it is full and metering each one kept.
fn consume_pcm(ring: &mut Consumer<f32>, pcm: &mut Pcm16, meter: &mut LevelMeter) {
    let Ok(chunk) = ring.read_chunk(ring.slots()) else {
        return;
    };
    let (first, second) = chunk.as_slices();
    for &sample in first.iter().chain(second).take(pcm.room()) {
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
        pcm.push(encoded);
    }
    chunk.commit_all();
}

/// Classifies the recording so far beneath every cue noticed since the last call, including the
/// cue's first samples, which may arrive before its notice.
fn classify(speech: &mut Audibility, audio: &[u8], cues: &Receiver<()>) {
    while cues.try_recv().is_ok() {
        speech.expect_cue(audio);
    }
    speech.extend(audio);
}

/// Whether nobody wants the recording any more: it was cancelled, or its owner has gone.
fn abandoned(control: &Control, events: &Sender<CaptureEvent>) -> bool {
    control.mode() == Mode::Cancelled || events.is_closed()
}

/// Maps RMS onto the meter's dBFS span, whose top is a speech reference rather than clipping.
fn speech_meter_level(rms: f32) -> f32 {
    let above_floor = 20.0 * rms.max(0.000_001).log10() - METER_FLOOR_DBFS;
    (above_floor / (METER_TOP_DBFS - METER_FLOOR_DBFS)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests;
