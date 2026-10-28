//! Short kalimba cues that confirm a gesture by ear: the microphone listening, a recording
//! stopping, a cancel, a Show starting, and something copied. An owned thread opens the default
//! output device as a session begins, so no cue waits for a device to open, and closes it once the
//! session and its last cue are ten seconds past, so an idle Speakeasy holds no audio stream. A cue
//! that cannot play, or cannot be heard in time, is skipped: sound only echoes what the pill shows.

use std::{
    io,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError},
    time::{Duration, Instant},
};

use cpal::{
    FromSample, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};
use speakeasy_platform::OwnedThread;

use crate::lowpass::Lowpass;

/// How long the microphone may hear a cue once the owner asks for it, so the speech gate listens
/// beneath cues for this long: the latest a cue may be heard, the longest cue a recording can
/// hear, and the room's echo.
pub(crate) const HEARD_FOR: Duration = LATE.saturating_add(HEARD_CUE).saturating_add(ECHO);
/// The latest a cue may be heard after it is asked for, which still matches the pill.
const LATE: Duration = Duration::from_millis(300);
/// The longest cue a recording can hear.
const HEARD_CUE: Duration = Duration::from_millis(200);
/// How long a room's echo keeps a cue audible after the cue ends, which also covers the delay
/// before the microphone delivers what it hears.
const ECHO: Duration = Duration::from_millis(100);
/// How long the output device stays open once nothing holds it and the last cue was asked for.
const LINGER: Duration = Duration::from_secs(10);
/// The sample rate of the cue recordings.
const RATE: u64 = 48_000;
/// Cues that can sound at once; another replaces the one that has played longest.
const VOICES: usize = 4;

/// A moment worth hearing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cue {
    /// The microphone is listening: a high note.
    Start,
    /// A recording ends and goes to recognition: a lower note.
    Stop,
    /// A recording or its processing is discarded: a soft low note.
    Cancel,
    /// A recording becomes a Show: two rising notes.
    Show,
    /// Dictation or a Show's summary waits on the clipboard to be pasted: a rising chord.
    Copied,
}

impl Cue {
    /// The recording: 48 kHz mono PCM16 samples after a canonical 44-byte WAV header.
    const fn wav(self) -> &'static [u8] {
        match self {
            Self::Start => START,
            Self::Stop => STOP,
            Self::Cancel => CANCEL,
            Self::Show => SHOW,
            Self::Copied => COPIED,
        }
    }

    /// The recording's samples: 48 kHz mono PCM16. The two a recording can hear, `Start` and
    /// `Show`, are high-passed at 400 Hz, which leaves the speech gate a voice band to hear.
    pub(crate) fn pcm(self) -> &'static [u8] {
        self.wav().get(WAV_HEADER..).unwrap_or_default()
    }
}

const START: &[u8] = include_bytes!("../assets/sounds/start.wav");
const STOP: &[u8] = include_bytes!("../assets/sounds/stop.wav");
const CANCEL: &[u8] = include_bytes!("../assets/sounds/cancel.wav");
const SHOW: &[u8] = include_bytes!("../assets/sounds/show.wav");
const COPIED: &[u8] = include_bytes!("../assets/sounds/copied.wav");
const WAV_HEADER: usize = 44;

const _: () = assert!(
    is_canonical(START)
        && is_canonical(STOP)
        && is_canonical(CANCEL)
        && is_canonical(SHOW)
        && is_canonical(COPIED),
    "every cue is a canonical 48 kHz mono PCM16 WAV"
);
const _: () = assert!(
    lasts_at_most(START, HEARD_CUE) && lasts_at_most(SHOW, HEARD_CUE),
    "the cues a recording can hear fit the span the speech gate listens beneath"
);

/// Whether the samples in `wav` play for no longer than `limit`.
const fn lasts_at_most(wav: &[u8], limit: Duration) -> bool {
    const BYTES_PER_MILLISECOND: u128 = RATE as u128 * 2 / 1000;
    wav.len().saturating_sub(WAV_HEADER) as u128
        <= limit.as_millis().saturating_mul(BYTES_PER_MILLISECOND)
}

/// Whether `wav` holds a 44-byte header for 48 kHz mono PCM16 and then exactly the samples it
/// declares, so the samples start at a fixed offset.
const fn is_canonical(wav: &[u8]) -> bool {
    let Some((header, samples)) = wav.split_first_chunk::<WAV_HEADER>() else {
        return false;
    };
    #[rustfmt::skip]
    let [
        b'R', b'I', b'F', b'F', r0, r1, r2, r3, b'W', b'A', b'V', b'E',
        // A 16-byte format chunk: PCM, one channel, 48,000 Hz, 96,000 bytes a second, 2-byte
        // frames of 16 bits.
        b'f', b'm', b't', b' ', 16, 0, 0, 0, 1, 0, 1, 0,
        0x80, 0xBB, 0, 0, 0x00, 0x77, 0x01, 0, 2, 0, 16, 0,
        b'd', b'a', b't', b'a', d0, d1, d2, d3,
    ] = *header
    else {
        return false;
    };
    let data = u32::from_le_bytes([d0, d1, d2, d3]) as u64;
    let riff = u32::from_le_bytes([r0, r1, r2, r3]) as u64;
    data == samples.len() as u64
        && samples.len().is_multiple_of(2)
        && riff == data.saturating_add(36)
}

/// What the owner asks of the player.
enum Ask {
    /// Sound the cue, asked for at that instant.
    Play(Cue, Instant),
    /// Open the output device and keep it open until released.
    Hold,
    /// Let the device close [`LINGER`] after the last ask.
    Release,
}

/// Plays cues on an owned thread, which keeps the output device open only through a hold and for
/// [`LINGER`] after it or the last cue.
pub(crate) struct Sounds {
    /// Declared first so it drops first, which ends the player before its thread is joined.
    asks: Sender<Ask>,
    _player: OwnedThread,
}

impl Sounds {
    pub(crate) fn start() -> io::Result<Self> {
        let (asks, asked) = mpsc::channel();
        let player = OwnedThread::spawn("sound-cues", move || {
            play(&asked, LINGER, Output::open);
        })?;
        Ok(Self {
            asks,
            _player: player,
        })
    }

    /// Plays `cue` now, or not at all.
    pub(crate) fn play(&self, cue: Cue) {
        self.ask(Ask::Play(cue, Instant::now()));
    }

    /// Opens the output device and keeps it open while `held`, so a cue never waits for one; a
    /// release closes it after [`LINGER`].
    pub(crate) fn hold(&self, held: bool) {
        self.ask(if held { Ask::Hold } else { Ask::Release });
    }

    fn ask(&self, ask: Ask) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Only a panicked player closes the lane, and a cue merely echoes the pill"
        )]
        let _ = self.asks.send(ask);
    }
}

/// Where the player sends cues: the output device, or a test's record of it.
trait Sink {
    /// Starts `cue` sounding, unless it would be heard after `deadline` or cues are already queued
    /// faster than the device plays them.
    fn start(&mut self, cue: Cue, deadline: Instant);

    /// Whether the device has failed, for example by being unplugged, so the next cue needs
    /// another.
    fn is_broken(&self) -> bool;
}

/// Plays asked cues until the lane closes, each by [`LATE`] after it was asked for. A sink opens
/// for a hold or a cue, and closes once it is not held and nothing has been asked for `linger`.
fn play<S: Sink>(asked: &Receiver<Ask>, linger: Duration, mut open: impl FnMut() -> Option<S>) {
    let mut sink: Option<S> = None;
    let mut held = false;
    let mut closing = Instant::now();
    loop {
        let next = if sink.is_some() && !held {
            asked.recv_timeout(closing.saturating_duration_since(Instant::now()))
        } else {
            asked.recv().map_err(RecvTimeoutError::from)
        };
        let ask = match next {
            Ok(ask) => ask,
            Err(RecvTimeoutError::Timeout) => {
                sink = None;
                continue;
            },
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let now = Instant::now();
        closing = now.checked_add(linger).unwrap_or(now);
        match ask {
            Ask::Hold => {
                held = true;
                sink = usable(sink, &mut open);
            },
            Ask::Release => held = false,
            Ask::Play(cue, at) => {
                sink = usable(sink, &mut open);
                if let Some(sink) = &mut sink {
                    sink.start(cue, at.checked_add(LATE).unwrap_or(at));
                }
            },
        }
    }
}

/// `sink` while it works; otherwise a replacement, opened only after the failed one is released.
fn usable<S: Sink>(sink: Option<S>, open: impl FnOnce() -> Option<S>) -> Option<S> {
    sink.filter(|sink| !sink.is_broken()).or_else(open)
}

/// The default output device, open through a hold or after a recent cue. Fields drop in declaration
/// order, so the stream stops before its device and host.
struct Output {
    asks: Producer<(Cue, Instant)>,
    failures: Receiver<cpal::Error>,
    _stream: cpal::Stream,
    _device: cpal::Device,
    _host: cpal::Host,
}

impl Output {
    /// Opens the default output device at its preferred format, or `None` when there is none it can
    /// play on.
    fn open() -> Option<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device()?;
        let format = device.default_output_config().ok()?;
        let rate = NonZeroU32::new(format.sample_rate())?;
        let channels = NonZeroUsize::new(usize::from(format.channels()))?;
        let (asks, asked) = RingBuffer::new(VOICES);
        let mixer = Mixer::new(Bank::render(rate), asked, channels);
        let (failed, failures) = mpsc::sync_channel(1);
        let config = format.config();
        let stream = match format.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config, mixer, failed),
            SampleFormat::I16 => build::<i16>(&device, config, mixer, failed),
            SampleFormat::U16 => build::<u16>(&device, config, mixer, failed),
            SampleFormat::I32 => build::<i32>(&device, config, mixer, failed),
            _ => None,
        }?;
        stream.play().ok()?;
        Some(Self {
            asks,
            failures,
            _stream: stream,
            _device: device,
            _host: host,
        })
    }
}

impl Sink for Output {
    fn start(&mut self, cue: Cue, deadline: Instant) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A full queue means the device has stalled, and a late cue is worth skipping"
        )]
        let _ = self.asks.push((cue, deadline));
    }

    fn is_broken(&self) -> bool {
        !matches!(self.failures.try_recv(), Err(TryRecvError::Empty))
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut mixer: Mixer,
    failed: SyncSender<cpal::Error>,
) -> Option<cpal::Stream> {
    device
        .build_output_stream(
            config,
            move |output: &mut [T], info: &cpal::OutputCallbackInfo| {
                mixer.fill(output, heard(Instant::now(), info.timestamp()));
            },
            move |error: cpal::Error| {
                if breaks(error.kind()) {
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "One reported failure already marks the device broken"
                    )]
                    let _ = failed.try_send(error);
                }
            },
            None,
        )
        .ok()
}

/// When what an output callback writes at `now` is heard: once the device's latency has passed.
fn heard(now: Instant, stamp: cpal::OutputStreamTimestamp) -> Instant {
    now.checked_add(stamp.playback.duration_since(stamp.callback))
        .unwrap_or(now)
}

/// Whether an error of `kind` stops the stream. A glitch, a reroute to the new default device, or
/// a refused real-time priority leaves it playing.
const fn breaks(kind: cpal::ErrorKind) -> bool {
    !matches!(
        kind,
        cpal::ErrorKind::Xrun | cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied
    )
}

/// Every cue rendered at one output rate.
struct Bank {
    start: Box<[f32]>,
    stop: Box<[f32]>,
    cancel: Box<[f32]>,
    show: Box<[f32]>,
    copied: Box<[f32]>,
}

impl Bank {
    fn render(rate: NonZeroU32) -> Self {
        let render = |cue: Cue| resample(cue.pcm(), rate);
        Self {
            start: render(Cue::Start),
            stop: render(Cue::Stop),
            cancel: render(Cue::Cancel),
            show: render(Cue::Show),
            copied: render(Cue::Copied),
        }
    }

    fn samples(&self, cue: Cue) -> &[f32] {
        match cue {
            Cue::Start => &self.start,
            Cue::Stop => &self.stop,
            Cue::Cancel => &self.cancel,
            Cue::Show => &self.show,
            Cue::Copied => &self.copied,
        }
    }
}

/// `pcm`, 48 kHz PCM16, as samples at `rate`, interpolated linearly between recorded samples. A
/// slower rate first loses what lies above 45% of it, falling 48 dB an octave, so what would fold
/// back into what it plays stays faint.
#[expect(
    clippy::cast_possible_truncation,
    reason = "Interpolated cue samples lie within full scale, which f32 holds far more finely than PCM16"
)]
fn resample(pcm: &[u8], rate: NonZeroU32) -> Box<[f32]> {
    let mut band = Lowpass::new(0.45 * f64::from(rate.get()), RATE as f64)
        .filter(|_| u64::from(rate.get()) < RATE);
    let recorded: Vec<f64> = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let sample = f64::from(i16::from_le_bytes(*pair)) / 32_768.0;
            band.as_mut().map_or(sample, |band| band.filter(sample))
        })
        .collect();
    let sample = |index: u64| {
        usize::try_from(index)
            .ok()
            .and_then(|index| recorded.get(index))
            .copied()
            .unwrap_or_default()
    };
    let rate = NonZeroU64::from(rate);
    let frames = (recorded.len() as u64).saturating_mul(rate.get()) / RATE;
    (0..frames)
        .map(|frame| {
            let position = frame.saturating_mul(RATE);
            let (index, offset) = (position / rate, position % rate);
            let (before, after) = (sample(index), sample(index.saturating_add(1)));
            (before + (after - before) * (offset as f64 / rate.get() as f64)) as f32
        })
        .collect()
}

/// The cues sounding, mixed onto every channel in the output callback without allocating or
/// blocking.
struct Mixer {
    bank: Bank,
    channels: NonZeroUsize,
    /// Cues to start, each with the latest instant it may be heard.
    asked: Consumer<(Cue, Instant)>,
    voices: [Option<Voice>; VOICES],
}

#[derive(Clone, Copy)]
struct Voice {
    cue: Cue,
    /// The next sample to play.
    at: usize,
}

impl Mixer {
    const fn new(bank: Bank, asked: Consumer<(Cue, Instant)>, channels: NonZeroUsize) -> Self {
        Self {
            bank,
            channels,
            asked,
            voices: [None; VOICES],
        }
    }

    /// Fills `output`, whose first frame is heard at `heard`, skipping cues asked to be heard
    /// sooner.
    fn fill<T: SizedSample + FromSample<f32>>(&mut self, output: &mut [T], heard: Instant) {
        while let Ok((cue, deadline)) = self.asked.pop() {
            if heard > deadline {
                continue;
            }
            if let Some(slot) = self
                .voices
                .iter_mut()
                .max_by_key(|slot| slot.map_or(usize::MAX, |voice| voice.at))
            {
                *slot = Some(Voice { cue, at: 0 });
            }
        }
        let Self {
            bank,
            channels,
            voices,
            ..
        } = self;
        for frame in output.chunks_mut(channels.get()) {
            let mut mixed = 0.0;
            for slot in voices.iter_mut() {
                let Some(voice) = slot else {
                    continue;
                };
                if let Some(sample) = bank.samples(voice.cue).get(voice.at) {
                    mixed += sample;
                    voice.at = voice.at.saturating_add(1);
                } else {
                    *slot = None;
                }
            }
            frame.fill(T::from_sample(mixed.clamp(-1.0, 1.0)));
        }
    }
}

#[cfg(test)]
mod tests;
