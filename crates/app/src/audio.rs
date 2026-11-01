mod checkpoint;
mod control;
mod cuts;
mod live;
mod microphones;
mod pcm;
mod speech;
mod stream;

#[cfg(test)]
pub(crate) mod fixture;

use std::{
    panic::{self, AssertUnwindSafe},
    sync::Arc,
    thread::{self, Thread},
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow};
use async_channel::{Receiver, Sender};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(test)]
pub(crate) use pcm::wav_duration;
use rtrb::{Consumer, RingBuffer};
use speakeasy_core::gesture::RECORDING_LIMIT;
use speakeasy_platform::OwnedThread;

pub(crate) use self::{
    checkpoint::seal_kept,
    microphones::{InputDevice, MicrophoneScan},
    pcm::{silent_wav, wav_sample_rate, wav_tail},
    stream::MAX_SAMPLE_RATE,
};
use self::{
    control::{Control, Mode},
    cuts::Cuts,
    pcm::{Pcm16, samples_in},
    speech::Audibility,
    stream::{CallbackState, build_stream, capture_failure, microphone_error, rate_and_channels},
};
use crate::runtime::ports::{CaptureEvent, Capturing, Listening, Sealed, SessionId};

const INITIAL_RESERVATION: Duration = Duration::from_secs(10);

const RING_CAPACITY: Duration = Duration::from_secs(1);
const METER_INTERVAL: Duration = Duration::from_millis(32);
const DRAIN_INTERVAL: Duration = Duration::from_millis(5);
const METER_FLOOR_DBFS: f32 = -60.0;
const METER_TOP_DBFS: f32 = -6.0;
const CAPTURE_STOPPED: &str = "Microphone stopped unexpectedly. Try recording again.";

pub(crate) struct Capture {
    signal: Signal,
    cues: Sender<()>,
    keep: Sender<checkpoint::Keep>,
    whole: Sender<()>,
    thread: OwnedThread,
}

impl Capture {
    pub(crate) fn start(
        id: SessionId,
        microphone: Option<&str>,
        recognition: crate::runtime::ports::RecognitionMode,
        events: Sender<CaptureEvent>,
    ) -> anyhow::Result<Self> {
        let microphone = microphone.map(str::to_owned);
        Self::spawn(
            id,
            events,
            move |events, control, cues, keep, whole| match Recorder::open(
                microphone.as_deref(),
                recognition,
                events,
                control,
            ) {
                Ok(recorder) => {
                    let Captured { outcome, device } =
                        recorder.record(id, events, control, cues, keep, whole);
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
            },
        )
    }

    fn spawn<D: 'static>(
        id: SessionId,
        events: Sender<CaptureEvent>,
        work: impl FnOnce(
            &Sender<CaptureEvent>,
            &Arc<Control>,
            &Receiver<()>,
            &Receiver<checkpoint::Keep>,
            &Receiver<()>,
        ) -> anyhow::Result<Captured<D>>
        + Send
        + 'static,
    ) -> anyhow::Result<Self> {
        let control = Arc::new(Control::default());
        let (cues, heard) = async_channel::unbounded();
        let (keep, kept) = async_channel::bounded(1);
        let (whole, entire) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("audio-capture", {
            let control = Arc::clone(&control);
            move || {
                let ran = panic::catch_unwind(AssertUnwindSafe(|| {
                    work(&events, &control, &heard, &kept, &entire)
                }));
                kept.close();
                drop(kept);
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
            keep,
            whole,
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
        async move {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The recording outcome is already decided; retirement waits for all native cleanup even after a panic"
            )]
            let _ = thread.join().await;
        }
    }
}

impl Listening for Capture {
    fn whole(&self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "One stop request is enough; a full or closed lane already prevents further work"
        )]
        let _ = self.whole.try_send(());
        self.signal.thread.unpark();
    }

    fn keep(&self, log: crate::show::AudioLog, start: Duration, language: &str) -> bool {
        let accepted = self
            .keep
            .try_send(checkpoint::Keep {
                log,
                start,
                language: language.into(),
            })
            .is_ok();
        self.signal.thread.unpark();
        accepted
    }

    fn expect_cue(&self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A capture that has ended has no speech left to gate"
        )]
        let _ = self.cues.try_send(());
    }
}

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

enum Startup {
    Stopped,
    Failed(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for Startup {
    fn from(error: E) -> Self {
        Self::Failed(error.into())
    }
}

struct Captured<D> {
    outcome: anyhow::Result<Option<Sealed>>,
    device: D,
}

struct LiveStream {
    _stream: cpal::Stream,
    _device: cpal::Device,
    _host: cpal::Host,
}

struct Recorder<D = LiveStream> {
    pcm: Pcm16,
    speech: Audibility,
    device: D,
    ring: Consumer<f32>,
    errors: Receiver<cpal::Error>,
    started: Instant,
    rate: u32,
    cuts: Option<Cuts>,
    live: live::Feed,
    #[cfg(test)]
    profile: Option<fixture::Profile>,
}

impl Recorder {
    fn open(
        microphone: Option<&str>,
        recognition: crate::runtime::ports::RecognitionMode,
        events: &Sender<CaptureEvent>,
        control: &Arc<Control>,
    ) -> Result<Self, Startup> {
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
            cuts: (recognition == crate::runtime::ports::RecognitionMode::Segmented)
                .then(|| Cuts::new(rate)),
            live: if recognition == crate::runtime::ports::RecognitionMode::Streaming {
                live::Feed::Waiting
            } else {
                live::Feed::Abandoned
            },
            #[cfg(test)]
            profile: None,
        })
    }
}

impl<D> Recorder<D> {
    fn record(
        mut self,
        id: SessionId,
        events: &Sender<CaptureEvent>,
        control: &Control,
        cues: &Receiver<()>,
        keep: &Receiver<checkpoint::Keep>,
        whole: &Receiver<()>,
    ) -> Captured<D> {
        let mut keeping = checkpoint::Keeping::new(keep);
        let drained = self.drain(id, events, control, cues, &mut keeping, whole);
        if !abandoned(control, events) && !keeping.finish(self.pcm.audio(), self.rate) {
            keeping.warn(id, events);
        }
        drop(keeping);
        control.finish();
        control.quiesce();
        let Self {
            pcm,
            mut speech,
            device,
            errors,
            rate,
            cuts,
            live,
            ..
        } = self;
        let outcome = drained.and_then(|()| {
            if abandoned(control, events) {
                return Ok(None);
            }

            capture_failure(control, &errors)?;
            speech.complete(pcm.audio());
            live.finish(pcm.audio());
            let partition = cuts.and_then(|cuts| cuts.finish(&speech, pcm.audio().len()));
            let mut sealed = pcm.seal(speech, rate)?;
            if let Some(sealed) = &mut sealed {
                sealed.partition = partition;
            }
            Ok(sealed)
        });
        Captured { outcome, device }
    }

    fn drain(
        &mut self,
        id: SessionId,
        events: &Sender<CaptureEvent>,
        control: &Control,
        cues: &Receiver<()>,
        keeping: &mut checkpoint::Keeping<'_>,
        whole: &Receiver<()>,
    ) -> anyhow::Result<()> {
        let mut meter = LevelMeter::new();
        let mut ready = false;
        loop {
            #[cfg(test)]
            if let Some(profile) = &mut self.profile {
                profile.polled();
            }
            if abandoned(control, events) {
                return Ok(());
            }
            if whole.try_recv().is_ok() {
                self.cuts = None;
                self.live = live::Feed::Abandoned;
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

            if !ready && self.pcm.samples() > 0 {
                ready = true;
                events.send_blocking(CaptureEvent::Ready(id))?;
            }
            keeping.classify(&mut self.speech, self.pcm.audio(), cues);
            if !keeping.offer(self.pcm.audio(), self.rate) {
                keeping.warn(id, events);
            }
            if let Some(level) = meter.take_level() {
                #[cfg(test)]
                if let Some(profile) = &mut self.profile {
                    profile.metered();
                }
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Meter updates are expendable presentation; a full or closed event lane must not interrupt audio capture"
                )]
                let _ = events.try_send(CaptureEvent::Level(id, level));
            }
            self.live
                .offer(id, events, self.pcm.audio(), &self.speech, self.rate);
            if stopping {
                return Ok(());
            }
            if !events.is_full()
                && let Some(cuts) = &mut self.cuts
                && let Some(range) = cuts.offer(&self.speech, self.pcm.audio().len())
            {
                let end = range.end;
                let Ok(piece) = self.pcm.excerpt(range, self.rate) else {
                    self.cuts = None;
                    continue;
                };
                if events
                    .try_send(CaptureEvent::Piece(id, cuts.number(), piece))
                    .is_ok()
                {
                    cuts.sent(end);
                }
            }

            thread::park_timeout(DRAIN_INTERVAL);
        }
    }
}

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

fn select_device(
    host: &cpal::Host,
    microphone: Option<&str>,
    checkpoint: impl Fn() -> Result<(), Startup>,
) -> Result<cpal::Device, Startup> {
    let Some(id) = microphone else {
        return Ok(host
            .default_input_device()
            .context("No microphone found. Connect one, then try again.")?);
    };
    for device in host.input_devices().map_err(microphone_error)? {
        checkpoint()?;
        if device.id().is_ok_and(|actual| actual.to_string() == id) {
            return Ok(device);
        }
    }
    checkpoint()?;
    Err(
        anyhow!("The selected microphone is disconnected. Reconnect it, or choose another microphone in Settings.")
            .into(),
    )
}

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

fn abandoned(control: &Control, events: &Sender<CaptureEvent>) -> bool {
    control.mode() == Mode::Cancelled || events.is_closed()
}

fn speech_meter_level(rms: f32) -> f32 {
    let above_floor = 20.0 * rms.max(0.000_001).log10() - METER_FLOOR_DBFS;
    (above_floor / (METER_TOP_DBFS - METER_FLOOR_DBFS)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests;
