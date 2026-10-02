//! Microphone capture on an owned thread. The real-time callback queues mono samples without
//! allocating; the capture thread encodes them in place behind a reserved WAV header.

mod control;
mod cuts;
mod speech;

use std::{
    convert::Infallible,
    mem,
    num::NonZeroUsize,
    ops::Range,
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

use self::{
    control::{Control, Mode},
    cuts::{AtPause, Cuts},
    speech::Speech,
};
use crate::{
    ports::Recording,
    runtime::{CaptureEvent, Captured, SessionId},
};

const WAV_HEADER_BYTES: usize = 44;
const SAMPLE_RATE_OFFSET: usize = 24;
const SAMPLE_BYTES: usize = 2;
const MAX_SAMPLE_RATE: u32 = 192_000;
const MAX_CHANNELS: usize = 32;
const INITIAL_RESERVATION: Duration = Duration::from_secs(10);
const MINIMUM_RECORDING: Duration = Duration::from_millis(200);
/// Speech since the previous segment that a pause commits as its own segment, so a long recording
/// is recognized while it continues and recognition stays linear in its length.
const SEGMENT: Duration = Duration::from_secs(20);
const METER_INTERVAL: Duration = Duration::from_millis(32);
/// How often the consumer drains the ring until the first samples arrive, so startup feedback is
/// prompt.
const STARTUP_DRAIN: Duration = Duration::from_millis(5);
/// How often it drains once audio flows: twice per meter interval, well inside the ring's second.
const DRAIN_INTERVAL: Duration = Duration::from_millis(16);
/// How soon a due pause mark is rechecked when its audio has not reached the ring yet.
const MARK_RECHECK: Duration = Duration::from_millis(2);
const METER_FLOOR_DBFS: f32 = -60.0;
const METER_TOP_DBFS: f32 = -6.0;
const RECLAIM_SLACK_BYTES: usize = 8 * 1024 * 1024;

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

    fn spawn<R: Into<Recorded>>(
        id: SessionId,
        events: Sender<CaptureEvent>,
        work: impl FnOnce(&Sender<CaptureEvent>, &Arc<Control>) -> R + Send + 'static,
    ) -> anyhow::Result<Self> {
        let control = Arc::new(Control::default());
        let (exit, alive) = async_channel::bounded::<Infallible>(1);
        let thread = thread::Builder::new().name("audio-capture".into()).spawn({
            let control = control.clone();
            move || {
                speakeasy_platform::prefer_responsive_thread();
                // A panicked capture's state is discarded; only its event lane is used afterwards.
                let Recorded { outcome, device } =
                    panic::catch_unwind(AssertUnwindSafe(|| work(&events, &control).into()))
                        .unwrap_or_else(|_| {
                            Recorded::from(Err(anyhow!(
                                "Microphone stopped unexpectedly. Try recording again."
                            )))
                        });
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A closed event lane belongs to a departed owner; retirement still observes this thread's exit"
                )]
                let _ = events.send_blocking(CaptureEvent::Finished(id, outcome));
                // Stopping the device after reporting keeps its teardown off the path from release
                // to recognition; retirement still waits for it below.
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

    fn speculate(&self) {
        self.signal.control.speculate();
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

/// What a capture leaves behind: its outcome, and the device to stop once that is reported.
struct Recorded {
    outcome: anyhow::Result<Option<Captured>>,
    device: Option<LiveStream>,
}

impl From<anyhow::Result<Option<Captured>>> for Recorded {
    fn from(outcome: anyhow::Result<Option<Captured>>) -> Self {
        Self {
            outcome,
            device: None,
        }
    }
}

/// A playing stream and what opened it. Fields drop in declaration order, so the stream stops
/// before the device and host.
struct LiveStream {
    _stream: cpal::Stream,
    _device: cpal::Device,
    _host: cpal::Host,
}

/// The audio a recording has delivered, which parts of it are speech, and which parts went to the
/// owner early. Live capture and the timing replay drive the same tracker; the audio is erased when
/// it drops.
pub(crate) struct Tracker {
    pcm: Pcm16,
    speech: Speech,
    /// Segments handed to the owner, and pauses offered for speculation.
    cuts: Cuts,
    rate: u32,
}

impl Tracker {
    fn new(rate: u32) -> anyhow::Result<Self> {
        Ok(Self {
            pcm: Pcm16::with_reservation(samples_in(INITIAL_RESERVATION, rate)),
            speech: Speech::new(rate).context("Unsupported microphone format")?,
            cuts: Cuts::new(bytes_in(samples_in(SEGMENT, rate))),
            rate,
        })
    }

    /// At a pause, hands the owner a copy of the speech since the previous segment: as a segment of
    /// its own once it is long enough, or otherwise, while speculating, as the tail a recording
    /// stopped now would hold. A full event lane forgoes this pause rather than blocking capture;
    /// an unsent segment stays in the tail.
    fn at_pause(
        &mut self,
        id: SessionId,
        events: &Sender<CaptureEvent>,
        speculating: bool,
    ) -> anyhow::Result<()> {
        self.speech.extend(self.pcm.audio());
        if self.cuts.resumed(self.speech.speech_end()) {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A lost withdrawal costs only an obsolete recognition; capture must not block on a full lane"
            )]
            let _ = events.try_send(CaptureEvent::Resumed(id));
        }
        let Some(pause) = self.speech.pause() else {
            return Ok(());
        };
        let speech_end = pause.speech_end;
        match self.cuts.at_pause(pause, speculating) {
            AtPause::Segment(index, segment) => {
                let wav = self.pcm.excerpt(segment.clone(), self.rate)?;
                match events.try_send(CaptureEvent::Segment(id, index, wav)) {
                    Ok(()) => self.cuts.segment_sent(&segment),
                    Err(unsent) => unsent.into_inner().erase(),
                }
            },
            AtPause::Speculate(sequence, tail) => {
                let wav = self.pcm.excerpt(tail, self.rate)?;
                match events.try_send(CaptureEvent::Paused(id, sequence, wav)) {
                    Ok(()) => self.cuts.speculation_sent(sequence, speech_end),
                    Err(unsent) => unsent.into_inner().erase(),
                }
            },
            AtPause::Nothing => {},
        }
        Ok(())
    }

    /// How long to sleep before draining again: the drain interval, or less when quiet follows
    /// speech and a pause mark falls due sooner, so the mark's speculation starts on time instead of
    /// up to an interval late. A mark whose audio has not arrived is rechecked shortly.
    fn next_wake(&self) -> Duration {
        let Some(bytes) = self.speech.quiet_until_next_mark() else {
            return DRAIN_INTERVAL;
        };
        let samples = u64::try_from(bytes / SAMPLE_BYTES).unwrap_or(u64::MAX);
        let micros = samples
            .saturating_mul(1_000_000)
            .checked_div(u64::from(self.rate))
            .unwrap_or(u64::MAX);
        Duration::from_micros(micros).clamp(MARK_RECHECK, DRAIN_INTERVAL)
    }

    /// The finished recording's tail after its last segment, or `None` without speech.
    fn finish(&mut self) -> anyhow::Result<Option<Captured>> {
        self.speech.complete(self.pcm.audio());
        let kept = self
            .speech
            .retained(self.pcm.audio().len())
            .filter(|_| self.pcm.samples() >= samples_in(MINIMUM_RECORDING, self.rate));
        let (tail, speculated) = self.cuts.finish(kept, self.speech.speech_end());
        let segments = self.cuts.segments();
        let Some(tail) = tail else {
            return Ok((segments > 0).then_some(Captured {
                wav: None,
                speculated,
                segments,
            }));
        };
        self.pcm.keep(tail);
        Ok(Some(Captured {
            wav: Some(mem::take(&mut self.pcm).into_wav(self.rate)?),
            speculated,
            segments,
        }))
    }
}

/// Public audio fed through capture's tracker in real time, then quiet until finished, so the owner
/// and engine see pauses and finishes at the instants a microphone would deliver them. It opens no
/// device.
#[cfg(test)]
pub(crate) struct Replay {
    control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
    /// Whether the owner may enable speculation; off replays the 0.3.3 request path.
    speculation: bool,
}

#[cfg(test)]
impl Replay {
    /// Starts feeding `wav`'s mono PCM16. `spoken` receives the instant its last audible window has
    /// been fed.
    pub(crate) fn start(
        id: SessionId,
        wav: &[u8],
        speculation: bool,
        events: Sender<CaptureEvent>,
        spoken: Sender<Instant>,
    ) -> anyhow::Result<Self> {
        let rate = wav_sample_rate(wav).context("Fixture is not a WAV")?;
        let samples: Vec<i16> = wav
            .get(WAV_HEADER_BYTES..)
            .unwrap_or_default()
            .as_chunks::<SAMPLE_BYTES>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect();
        let mut classifier = Speech::new(rate).context("Unsupported fixture rate")?;
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        classifier.complete(&bytes);
        let last_word = classifier.speech_end() / SAMPLE_BYTES;
        let control = Arc::new(Control::default());
        let feeding = control.clone();
        let thread = thread::spawn(move || {
            let outcome = feed(id, rate, &samples, last_word, &events, &feeding, &spoken);
            let _sent = events.send_blocking(CaptureEvent::Finished(id, outcome));
        });
        Ok(Self {
            control,
            thread: Some(thread),
            speculation,
        })
    }
}

#[cfg(test)]
fn feed(
    id: SessionId,
    rate: u32,
    samples: &[i16],
    last_word: usize,
    events: &Sender<CaptureEvent>,
    control: &Control,
    spoken: &Sender<Instant>,
) -> anyhow::Result<Option<Captured>> {
    let mut tracker = Tracker::new(rate)?;
    let limit = usize::try_from(rate)?.saturating_mul(300);
    let per_second = u128::from(rate);
    let started = Instant::now();
    events.send_blocking(CaptureEvent::Ready(id, Duration::ZERO))?;
    let mut fed = 0_usize;
    loop {
        match control.mode() {
            Mode::Cancelled => return Ok(None),
            Mode::Finishing => return tracker.finish(),
            Mode::Recording => {},
        }
        let target = usize::try_from(
            started
                .elapsed()
                .as_micros()
                .saturating_mul(per_second)
                .checked_div(1_000_000)
                .unwrap_or_default(),
        )?;
        while fed < target {
            tracker
                .pcm
                .push(samples.get(fed).copied().unwrap_or(0), limit);
            fed = fed.saturating_add(1);
            if fed == last_word {
                spoken.try_send(Instant::now())?;
            }
        }
        tracker.at_pause(id, events, control.speculating())?;
        // Finish and cancel unpark this thread at once, as they do the live consumer.
        thread::park_timeout(tracker.next_wake());
    }
}

#[cfg(test)]
impl Recording for Replay {
    fn finish(&self) {
        self.control.finish();
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }

    fn speculate(&self) {
        if self.speculation {
            self.control.speculate();
        }
    }

    async fn retire(mut self) {
        self.control.cancel();
        if let Some(thread) = self.thread.take() {
            let _joined = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

/// A playing stream and the audio it has delivered so far.
struct Recorder {
    tracker: Tracker,
    device: LiveStream,
    ring: Consumer<f32>,
    errors: Receiver<cpal::Error>,
    started: Instant,
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
        let tracker = Tracker::new(rate)?;
        checkpoint()?;
        stream.play().map_err(microphone_error)?;
        Ok(Self {
            tracker,
            device: LiveStream {
                _stream: stream,
                _device: device,
                _host: host,
            },
            ring,
            errors: stream_errors,
            started,
            limit,
        })
    }

    /// Delivers audio until finished, then hands back its outcome with the still-running device;
    /// `opened` is how long the device took to start.
    fn record(
        mut self,
        id: SessionId,
        opened: Duration,
        events: &Sender<CaptureEvent>,
        control: &Control,
    ) -> Recorded {
        let outcome = match self.drain(id, opened, events, control) {
            Ok(true) => self.finalize(control),
            Ok(false) => Ok(None),
            Err(error) => Err(error),
        };
        Recorded {
            outcome,
            device: Some(self.device),
        }
    }

    /// Moves audio from the ring until the recording stops, returning whether it finished rather
    /// than being cancelled.
    fn drain(
        &mut self,
        id: SessionId,
        opened: Duration,
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
                || self.tracker.pcm.samples() >= self.limit;
            if stopping {
                // The stream keeps running until the outcome is reported, so wait out any callback
                // still publishing a packet it began before the finish; later ones publish nothing.
                control.finish();
                control.quiesce();
            }
            if !ready && !self.ring.is_empty() {
                ready = true;
                events.send_blocking(CaptureEvent::Ready(id, opened))?;
            }
            consume_pcm(
                &mut self.ring,
                &mut self.tracker.pcm,
                &mut meter,
                self.limit,
            );
            // The recording's own audio is moments away once it stops, so a last pause is not offered.
            self.tracker
                .at_pause(id, events, control.speculating() && !stopping)?;
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
            thread::park_timeout(if ready {
                self.tracker.next_wake()
            } else {
                STARTUP_DRAIN
            });
        }
    }

    fn finalize(&mut self, control: &Control) -> anyhow::Result<Option<Captured>> {
        if control.mode() == Mode::Cancelled {
            return Ok(None);
        }
        // A callback may have failed between the last poll and stream teardown.
        capture_failure(control, &self.errors)?;
        self.tracker.finish()
    }
}

/// Mono PCM16 behind a reserved WAV header, so encoding never copies the audio. Audio that is not
/// encoded is zeroed on drop, best effort: copies in the ring, allocator, HTTP client, or engine
/// remain.
#[derive(Default)]
struct Pcm16 {
    bytes: Vec<u8>,
}

impl Pcm16 {
    fn with_reservation(samples: usize) -> Self {
        let mut bytes = Vec::with_capacity(WAV_HEADER_BYTES.saturating_add(bytes_in(samples)));
        bytes.resize(WAV_HEADER_BYTES, 0);
        Self { bytes }
    }

    fn silence(samples: usize) -> Self {
        Self {
            bytes: vec![0; WAV_HEADER_BYTES.saturating_add(bytes_in(samples))],
        }
    }

    fn audio(&self) -> &[u8] {
        self.bytes.get(WAV_HEADER_BYTES..).unwrap_or_default()
    }

    fn samples(&self) -> usize {
        self.audio().len() / SAMPLE_BYTES
    }

    /// Appends a sample, doubling capacity when full but never past `limit` samples, so a recording
    /// at the limit cannot hold twice its size. Growth happens here, never in the audio callback.
    fn push(&mut self, sample: i16, limit: usize) {
        if self.bytes.len().saturating_add(SAMPLE_BYTES) > self.bytes.capacity() {
            let full = WAV_HEADER_BYTES.saturating_add(bytes_in(limit));
            let capacity = self.bytes.capacity().saturating_mul(2).min(full);
            self.bytes
                .reserve_exact(capacity.saturating_sub(self.bytes.len()));
        }
        self.bytes.extend_from_slice(&sample.to_le_bytes());
    }

    /// Keeps only `audio`, releasing a reservation the remainder no longer needs.
    fn keep(&mut self, audio: Range<usize>) {
        self.retain_audio(audio);
        self.release_excess_capacity();
    }

    /// Trims quiet edges as a recording stopped now would, and reports whether enough audible audio
    /// remains to transcribe.
    #[cfg(test)]
    fn trim_quiet_edges(&mut self, rate: u32) -> bool {
        let Some(mut speech) = Speech::new(rate) else {
            return false;
        };
        speech.complete(self.audio());
        let Some(range) = speech.retained(self.audio().len()) else {
            return false;
        };
        self.keep(range);
        true
    }

    /// A WAV holding a copy of `audio`.
    fn excerpt(&self, audio: Range<usize>, rate: u32) -> anyhow::Result<Vec<u8>> {
        let audio = self
            .audio()
            .get(audio)
            .context("Pause lies outside the recording")?;
        let header = wav_header(audio.len(), rate).context("Recording format is unsupported")?;
        let mut wav = Vec::with_capacity(WAV_HEADER_BYTES.saturating_add(audio.len()));
        wav.extend_from_slice(&header);
        wav.extend_from_slice(audio);
        Ok(wav)
    }

    fn retain_audio(&mut self, audio: Range<usize>) {
        let length = audio.len();
        if audio.start != 0 {
            let source = WAV_HEADER_BYTES.saturating_add(audio.start)
                ..WAV_HEADER_BYTES.saturating_add(audio.end);
            self.bytes.copy_within(source, WAV_HEADER_BYTES);
        }
        self.bytes.truncate(WAV_HEADER_BYTES.saturating_add(length));
    }

    /// Ordinary utterances stay allocation-free; only a large reservation that the remaining audio
    /// fills to at most a quarter is reallocated.
    fn release_excess_capacity(&mut self) {
        let capacity = self.bytes.capacity();
        if capacity.saturating_sub(self.bytes.len()) >= RECLAIM_SLACK_BYTES
            && capacity / 4 >= self.bytes.len()
        {
            self.bytes.shrink_to_fit();
        }
    }

    fn into_wav(mut self, rate: u32) -> anyhow::Result<Vec<u8>> {
        let Some((header, audio)) = self.bytes.split_first_chunk_mut::<WAV_HEADER_BYTES>() else {
            bail!("Recording header is incomplete. Try recording again.");
        };
        let Some(encoded) = wav_header(audio.len(), rate) else {
            bail!(
                "Recording format is unsupported. Choose another microphone in Settings and try again."
            );
        };
        *header = encoded;
        Ok(mem::take(&mut self.bytes))
    }
}

impl Drop for Pcm16 {
    fn drop(&mut self) {
        self.bytes.fill(0);
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

/// A microphone Settings can offer.
pub struct InputDevice {
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
) -> Recorded {
    let begun = Instant::now();
    match Recorder::open(microphone, events, control) {
        Ok(recorder) => recorder.record(id, begun.elapsed(), events, control),
        Err(Startup::Stopped) => Recorded::from(Ok(None)),
        Err(Startup::Failed(error)) => Recorded::from(Err(error)),
    }
}

/// Pays the audio system's first use in this process, 25-35 ms on an M4 Pro, on its own thread
/// before the first recording waits on it. Only the default input device is read, as listing
/// microphones does; no stream opens, so input never starts and the microphone indicator stays off.
pub(crate) fn prepare_capture() {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Warming is best effort; the first recording opens its device and reports any failure"
    )]
    let _ = thread::Builder::new()
        .name("audio-warmup".into())
        .spawn(|| {
            let _device = cpal::default_host().default_input_device();
        });
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
        move |error| handle_stream_error(error, &errors, &error_control),
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
        // A discontinuity before the first packet cannot lose queued speech. Later, an Xrun may have
        // dropped words, so it fails the recording.
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

/// The canonical header for `data_bytes` of mono PCM16, if they can be described.
fn wav_header(data_bytes: usize, rate: u32) -> Option<[u8; WAV_HEADER_BYTES]> {
    // A RIFF chunk's size excludes its own four-byte ID and four-byte size.
    const CHUNK_HEADER_BYTES: usize = 8;
    const FORMAT_CHUNK_BYTES: u32 = 16;
    const PCM: u16 = 1;
    const MONO: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 16;
    if rate == 0 || !data_bytes.is_multiple_of(SAMPLE_BYTES) {
        return None;
    }
    let riff_bytes = data_bytes.checked_add(WAV_HEADER_BYTES - CHUNK_HEADER_BYTES)?;
    let block_align = u16::try_from(SAMPLE_BYTES).ok()?;
    let byte_rate = rate.checked_mul(u32::from(block_align))?;
    let fields: [&[u8]; 13] = [
        b"RIFF",
        &u32::try_from(riff_bytes).ok()?.to_le_bytes(),
        b"WAVE",
        b"fmt ",
        &FORMAT_CHUNK_BYTES.to_le_bytes(),
        &PCM.to_le_bytes(),
        &MONO.to_le_bytes(),
        &rate.to_le_bytes(),
        &byte_rate.to_le_bytes(),
        &block_align.to_le_bytes(),
        &BITS_PER_SAMPLE.to_le_bytes(),
        b"data",
        &u32::try_from(data_bytes).ok()?.to_le_bytes(),
    ];
    let mut header = [0; WAV_HEADER_BYTES];
    for (byte, field) in header.iter_mut().zip(fields.into_iter().flatten()) {
        *byte = *field;
    }
    Some(header)
}

/// The sample rate field of a header written by this module.
pub(crate) fn wav_sample_rate(wav: &[u8]) -> Option<u32> {
    let field = wav.get(SAMPLE_RATE_OFFSET..)?.first_chunk()?;
    Some(u32::from_le_bytes(*field))
}

/// The PCM16 samples behind a header written by this module.
pub(crate) fn wav_pcm(wav: &[u8]) -> Option<&[u8]> {
    wav.get(WAV_HEADER_BYTES..)
}

/// How much audio a WAV written by this module holds.
pub(crate) fn wav_duration(wav: &[u8]) -> Option<Duration> {
    let samples = wav_pcm(wav)?.len() / SAMPLE_BYTES;
    let rate = wav_sample_rate(wav).filter(|&rate| rate > 0)?;
    Some(Duration::from_secs_f64(samples as f64 / f64::from(rate)))
}

pub(crate) fn silent_wav(rate: u32, duration: Duration) -> anyhow::Result<Vec<u8>> {
    Pcm16::silence(samples_in(duration, rate)).into_wav(rate)
}

fn samples_in(duration: Duration, rate: u32) -> usize {
    let samples = u128::from(rate).saturating_mul(duration.as_millis()) / 1000;
    usize::try_from(samples).unwrap_or(usize::MAX)
}

fn bytes_in(samples: usize) -> usize {
    samples.saturating_mul(SAMPLE_BYTES)
}

/// Lists input devices without opening a stream. Drivers may block, so enumeration runs on its own
/// detached thread.
///
/// # Errors
/// Returns an error if the audio host cannot enumerate its devices.
pub async fn microphones() -> anyhow::Result<Vec<InputDevice>> {
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

    fn pcm_from(segments: &[(usize, i16)]) -> Pcm16 {
        let mut pcm = Pcm16::with_reservation(0);
        for &(samples, amplitude) in segments {
            for _ in 0..samples {
                pcm.push(amplitude, usize::MAX);
            }
        }
        pcm
    }

    #[test]
    fn wav_encoding_rewrites_the_header_in_place_with_exact_riff_lengths() -> anyhow::Result<()> {
        let mut pcm = Pcm16 {
            bytes: vec![0xFF; WAV_HEADER_BYTES],
        };
        for sample in [i16::MIN, 0, i16::MAX] {
            pcm.push(sample, usize::MAX);
        }
        let allocation = pcm.bytes.as_ptr();
        let encoded = pcm.into_wav(16_000)?;
        assert_eq!(encoded.as_ptr(), allocation);
        assert_eq!(
            &encoded[..WAV_HEADER_BYTES],
            &[
                b'R', b'I', b'F', b'F', 42, 0, 0, 0, b'W', b'A', b'V', b'E', b'f', b'm', b't',
                b' ', 16, 0, 0, 0, 1, 0, 1, 0, 0x80, 0x3E, 0, 0, 0, 0x7D, 0, 0, 2, 0, 16, 0, b'd',
                b'a', b't', b'a', 6, 0, 0, 0,
            ]
        );
        assert_eq!(&encoded[WAV_HEADER_BYTES..], &[0, 0x80, 0, 0, 0xFF, 0x7F]);
        Ok(())
    }

    #[test]
    fn wav_encoding_rejects_incomplete_headers_partial_samples_and_invalid_rates() {
        let incomplete = "Recording header is incomplete";
        let unsupported = "Recording format is unsupported";
        for (length, rate, expected) in [
            (WAV_HEADER_BYTES - 1, 16_000, incomplete),
            (WAV_HEADER_BYTES + 1, 16_000, unsupported),
            (WAV_HEADER_BYTES + 2, 0, unsupported),
            (WAV_HEADER_BYTES + 2, u32::MAX, unsupported),
        ] {
            let pcm = Pcm16 {
                bytes: vec![0x5A; length],
            };
            let error = pcm.into_wav(rate).unwrap_err().to_string();
            assert!(
                error.starts_with(expected),
                "{length} bytes at {rate} Hz: {error}"
            );
        }
    }

    #[test]
    fn silence_round_trips_its_sample_rate() -> anyhow::Result<()> {
        let silence = silent_wav(16_000, Duration::from_secs(1))?;
        assert_eq!(wav_sample_rate(&silence), Some(16_000));
        assert_eq!(silence.len(), WAV_HEADER_BYTES + 32_000);
        assert_eq!(wav_sample_rate(&silence[..27]), None);
        Ok(())
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
                record(SessionId::FIRST, None, &events, &control)
                    .outcome?
                    .is_none()
            );
        }
        let (events, _audio) = async_channel::bounded(1);
        events.close();
        let control = Arc::new(Control::default());
        assert!(
            record(SessionId::FIRST, None, &events, &control)
                .outcome?
                .is_none()
        );
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
            Ok(None)
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
            Ok(None)
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
        events.try_send(CaptureEvent::Ready(SessionId::FIRST, Duration::ZERO))?;
        let capture = Capture::spawn(SessionId::FIRST, events, |_, _| Ok(None))?;
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
        let capture = Capture::spawn(SessionId::FIRST, events, |_, _| -> Recorded {
            panic::resume_unwind(Box::new(()));
        })?;
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
                    black_box(&pcm.bytes);
                    pcm.bytes.truncate(WAV_HEADER_BYTES);
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
        let rate = 48_000;
        let fixture = pcm_from(&[(rate, 0), (rate * 297, 3000), (rate * 2, 0)]);
        let mut timings = Vec::new();
        for _ in 0..21 {
            let mut pcm = Pcm16 {
                bytes: fixture.bytes.clone(),
            };
            let started = Instant::now();
            assert!(black_box(&mut pcm).trim_quiet_edges(48_000));
            timings.push(started.elapsed().as_secs_f64() * 1000.0);
            black_box(&pcm.bytes);
        }
        timings.sort_by(f64::total_cmp);
        eprintln!(
            "audio_trim: seconds=300 median_ms={:.3} p95_ms={:.3} runs=21",
            timings[10], timings[19]
        );
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

    #[test]
    fn quiet_edges_keep_word_padding_and_interior_pauses_but_clicks_are_rejected() {
        let rate = 16_000;
        let mut pcm = pcm_from(&[
            (rate * 2, 0),
            (rate, 3000),
            (rate * 3, 0),
            (rate, 3000),
            (rate * 2, 0),
        ]);
        let expected = pcm.audio()[rate * 3..rate * 15].to_vec();
        assert!(pcm.trim_quiet_edges(u32::try_from(rate).unwrap()));
        assert_eq!(pcm.audio(), expected);

        let mut click = pcm_from(&[(rate / 20, 3000), (rate * 2 - rate / 20, 0)]);
        assert!(!click.trim_quiet_edges(u32::try_from(rate).unwrap()));
    }

    #[test]
    fn a_pause_excerpt_is_byte_for_byte_the_recording_stopped_there() -> anyhow::Result<()> {
        let rate = 44_100;
        let mut pcm = pcm_from(&[(rate * 2, 0), (rate, 3000), (rate / 2, 0)]);
        let mut speech = Speech::new(u32::try_from(rate)?).context("Unsupported rate")?;
        speech.complete(pcm.audio());
        let range = speech
            .retained(pcm.audio().len())
            .context("No speech found")?;
        assert_ne!(range.start, 0, "The fixture must trim leading quiet");
        let excerpt = pcm.excerpt(range.clone(), u32::try_from(rate)?)?;
        pcm.keep(range);
        assert_eq!(excerpt, pcm.into_wav(u32::try_from(rate)?)?);
        Ok(())
    }

    #[test]
    fn sparse_long_recording_releases_capacity_without_changing_padded_audio() {
        let rate = 48_000;
        let mut pcm = Pcm16 {
            bytes: vec![0; WAV_HEADER_BYTES + rate * 300 * 2],
        };
        let speech = WAV_HEADER_BYTES + rate * 40 * 2..WAV_HEADER_BYTES + rate * 50 * 2;
        for pair in pcm.bytes[speech].as_chunks_mut::<2>().0 {
            pair.copy_from_slice(&3000_i16.to_le_bytes());
        }
        let expected = pcm.audio()[rate * 79..rate * 101].to_vec();
        assert!(pcm.trim_quiet_edges(u32::try_from(rate).unwrap()));
        assert_eq!(pcm.audio(), expected);
        assert_eq!(pcm.bytes.capacity(), pcm.bytes.len());
    }

    #[test]
    fn trimmed_capacity_is_kept_unless_both_slack_thresholds_are_met() {
        for (capacity, length) in [
            (1024 * 1024, WAV_HEADER_BYTES),
            (8 * 1024 * 1024, 1024 * 1024),
            (12 * 1024 * 1024, 4 * 1024 * 1024),
        ] {
            let mut bytes = Vec::with_capacity(capacity);
            bytes.resize(length, 0x5A);
            let allocation = bytes.as_ptr();
            let mut pcm = Pcm16 { bytes };
            pcm.release_excess_capacity();
            assert_eq!(pcm.bytes.capacity(), capacity);
            assert_eq!(pcm.bytes.as_ptr(), allocation);
            assert_eq!(pcm.bytes.len(), length);
            assert!(pcm.bytes.iter().all(|&byte| byte == 0x5A));
        }
    }
}
