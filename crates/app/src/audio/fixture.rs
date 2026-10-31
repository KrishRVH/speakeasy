//! Opt-in public PCM replay through the production callbacks, ring, drain, cutter, and sealing.
//! The producer owns only fixture samples and a thread; it never opens an input device.

use std::num::NonZeroUsize;

use serde::Serialize;

use super::*;

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct Statistics {
    pub max_drain_ms: f64,
    pub max_meter_ms: f64,
    pub meter_updates: u64,
}

pub(super) struct Profile {
    polled: Instant,
    metered: Instant,
    stats: Statistics,
    report: Sender<Statistics>,
}

impl Profile {
    pub(super) fn polled(&mut self) {
        self.stats.max_drain_ms = self
            .stats
            .max_drain_ms
            .max(self.polled.elapsed().as_secs_f64() * 1000.0);
        self.polled = Instant::now();
    }

    pub(super) fn metered(&mut self) {
        self.stats.max_meter_ms = self
            .stats
            .max_meter_ms
            .max(self.metered.elapsed().as_secs_f64() * 1000.0);
        self.stats.meter_updates = self.stats.meter_updates.saturating_add(1);
        self.metered = Instant::now();
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A failed profile may have stopped listening; capture cleanup still completes"
        )]
        let _ = self.report.try_send(self.stats.clone());
    }
}

/// Replays a canonical mono PCM16 WAV at `speed` times real time. Its completion lane timestamps
/// the last callback, before finishing capture; the drain statistics arrive independently.
pub(crate) fn replay(
    id: SessionId,
    wav: &[u8],
    segmenting: bool,
    speed: u32,
    events: Sender<CaptureEvent>,
    finished: Sender<Instant>,
    report: Sender<Statistics>,
) -> anyhow::Result<Capture> {
    let rate = wav_sample_rate(wav)?;
    anyhow::ensure!(
        rate > 0
            && speed > 0
            && wav.get(0..4) == Some(b"RIFF")
            && wav.get(20..24) == Some(&[1, 0, 1, 0])
            && wav.get(34..36) == Some(&[16, 0])
            && wav.get(36..40) == Some(b"data"),
        "The fixture needs a canonical mono PCM16 WAV"
    );
    let samples = wav
        .get(44..)
        .context("Fixture WAV has no payload")?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| i16::from_le_bytes(*bytes))
        .collect::<Vec<_>>();
    anyhow::ensure!(!samples.is_empty(), "The fixture has no samples");
    Capture::spawn(id, events, move |events, control, cues, keep, whole| {
        let started = Instant::now();
        let (producer, ring) = RingBuffer::new(samples_in(RING_CAPACITY, rate));
        let (errors, received) = async_channel::bounded(1);
        let limit = samples_in(RECORDING_LIMIT, rate);
        let callbacks = CallbackState {
            channels: NonZeroUsize::MIN,
            producer,
            control: Arc::clone(control),
            errors,
            started,
            limit,
        };
        let (callback, _errors) = stream::capture_callbacks::<i16>(callbacks);
        let drain = thread::current();
        let producer = OwnedThread::spawn("public-audio-fixture", {
            let control = Arc::clone(control);
            move || {
                if !feed(&control, callback, &samples, rate, speed, started) {
                    return;
                }
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A cancelled profiling run may no longer need its completion timestamp"
                )]
                let _ = finished.try_send(Instant::now());
                control.finish();
                drain.unpark();
            }
        })?;
        let recorder = Recorder {
            pcm: Pcm16::with_reservation(samples_in(INITIAL_RESERVATION, rate), limit),
            speech: Audibility::new(rate),
            device: producer,
            ring,
            errors: received,
            started,
            rate,
            cuts: segmenting.then(|| Cuts::new(rate)),
            profile: Some(Profile {
                polled: started,
                metered: started,
                stats: Statistics::default(),
                report,
            }),
        };
        Ok(recorder.record(id, events, control, cues, keep, whole))
    })
}

fn feed(
    control: &Control,
    mut callback: impl FnMut(&[i16]),
    samples: &[i16],
    rate: u32,
    speed: u32,
    started: Instant,
) -> bool {
    let mut sent = 0_usize;
    for chunk in samples.chunks(samples_in(DRAIN_INTERVAL, rate).max(1)) {
        if control.mode() != Mode::Recording {
            return false;
        }
        callback(chunk);
        sent = sent.saturating_add(chunk.len());
        let elapsed = Duration::from_secs_f64(sent as f64 / f64::from(rate) / f64::from(speed));
        if let Some(remaining) = elapsed.checked_sub(started.elapsed()) {
            thread::sleep(remaining);
        }
    }
    true
}
