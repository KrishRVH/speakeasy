//! Checks rendering, mixing, and the player's device lifecycle against a fake sink; only the opt-in
//! test opens an output device.

use std::{f64::consts::TAU, thread};

use super::*;

const PATIENCE: Duration = Duration::from_secs(2);
/// How long the players under test keep a released device open.
const LINGER: Duration = Duration::from_millis(50);

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Opened(u8),
    Started(u8, Cue, Instant),
    Dropped(u8),
}

struct FakeSink {
    id: u8,
    broken: bool,
    events: Sender<Event>,
}

impl Sink for FakeSink {
    fn start(&mut self, cue: Cue, deadline: Instant) {
        self.events
            .send(Event::Started(self.id, cue, deadline))
            .unwrap();
    }

    fn is_broken(&self) -> bool {
        self.broken
    }
}

impl Drop for FakeSink {
    fn drop(&mut self) {
        self.events.send(Event::Dropped(self.id)).unwrap();
    }
}

/// A player on its own thread whose sinks report to the returned lane; each sink is broken when
/// `broken` says so for its number, counting from 1.
fn player(broken: fn(u8) -> bool) -> (Sender<Ask>, Receiver<Event>, thread::JoinHandle<()>) {
    let (asks, asked) = mpsc::channel();
    let (events, reports) = mpsc::channel();
    let thread = thread::spawn(move || {
        let mut opened = 0_u8;
        play(&asked, LINGER, || {
            opened = opened.saturating_add(1);
            events.send(Event::Opened(opened)).unwrap();
            Some(FakeSink {
                id: opened,
                broken: broken(opened),
                events: events.clone(),
            })
        });
    });
    (asks, reports, thread)
}

fn next(reports: &Receiver<Event>) -> Event {
    reports.recv_timeout(PATIENCE).unwrap()
}

/// Asserts that the player reports nothing for several of its lingers.
fn quiet(reports: &Receiver<Event>, why: &str) {
    assert_eq!(
        reports.recv_timeout(LINGER.saturating_mul(4)),
        Err(RecvTimeoutError::Timeout),
        "{why}"
    );
}

/// When a cue asked for at `at` must be heard by.
fn by(at: Instant) -> Instant {
    at.checked_add(LATE).unwrap()
}

#[test]
fn the_device_closes_after_lingering_and_reopens_for_the_next_cue() {
    let (asks, reports, thread) = player(|_| false);
    let first = Instant::now();
    asks.send(Ask::Play(Cue::Start, first)).unwrap();
    assert_eq!(next(&reports), Event::Opened(1));
    assert_eq!(next(&reports), Event::Started(1, Cue::Start, by(first)));
    assert_eq!(next(&reports), Event::Dropped(1));
    let second = Instant::now();
    asks.send(Ask::Play(Cue::Stop, second)).unwrap();
    assert_eq!(next(&reports), Event::Opened(2));
    assert_eq!(next(&reports), Event::Started(2, Cue::Stop, by(second)));
    drop(asks);
    assert_eq!(next(&reports), Event::Dropped(2));
    thread.join().unwrap();
}

#[test]
fn a_held_device_opens_before_any_cue_and_closes_only_once_released() {
    let (asks, reports, thread) = player(|_| false);
    asks.send(Ask::Hold(true)).unwrap();
    assert_eq!(next(&reports), Event::Opened(1));
    quiet(&reports, "A held device closed before its session's cue");
    let asked = Instant::now();
    asks.send(Ask::Play(Cue::Stop, asked)).unwrap();
    assert_eq!(next(&reports), Event::Started(1, Cue::Stop, by(asked)));
    quiet(&reports, "A cue released a held device");
    asks.send(Ask::Hold(false)).unwrap();
    assert_eq!(next(&reports), Event::Dropped(1));
    drop(asks);
    thread.join().unwrap();
}

#[test]
fn a_broken_device_is_released_before_its_replacement_opens() {
    let (asks, reports, thread) = player(|sink| sink == 1);
    let asked = Instant::now();
    asks.send(Ask::Play(Cue::Start, asked)).unwrap();
    asks.send(Ask::Play(Cue::Stop, asked)).unwrap();
    drop(asks);
    thread.join().unwrap();
    let events: Vec<_> = reports.try_iter().collect();
    assert_eq!(
        events,
        [
            Event::Opened(1),
            Event::Started(1, Cue::Start, by(asked)),
            Event::Dropped(1),
            Event::Opened(2),
            Event::Started(2, Cue::Stop, by(asked)),
            Event::Dropped(2),
        ]
    );
}

#[test]
fn recordings_resample_by_interpolating_between_samples() {
    let ramp: Vec<u8> = [0_i16, 4096, 8192, 12288]
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    let native = resample(&ramp, NonZeroU32::new(48_000).unwrap());
    assert_eq!(*native, [0.0, 0.125, 0.25, 0.375]);
    let doubled = resample(&ramp, NonZeroU32::new(96_000).unwrap());
    assert_eq!(
        *doubled,
        [0.0, 0.0625, 0.125, 0.1875, 0.25, 0.3125, 0.375, 0.1875]
    );
    // 44.1 kHz keeps the recording's length in time.
    let cue = Cue::Copied.pcm();
    let rendered = resample(cue, NonZeroU32::new(44_100).unwrap());
    assert_eq!(rendered.len(), cue.len() / 2 * 44_100 / 48_000);
}

/// A second of a half-scale sine at `hz`, recorded at 48 kHz.
#[expect(
    clippy::cast_possible_truncation,
    reason = "A half-scale sine stays within the i16 range"
)]
fn recorded_tone(hz: f64) -> Vec<u8> {
    (0..48_000)
        .flat_map(|index| {
            let phase = TAU * hz * f64::from(index) / 48_000.0;
            ((phase.sin() * 16_384.0) as i16).to_le_bytes()
        })
        .collect()
}

#[test]
fn a_slower_output_drops_what_would_fold_back_into_its_band() {
    // A Bluetooth headset's microphone often turns its output down to 16 kHz.
    let rate = NonZeroU32::new(16_000).unwrap();
    let rms = |samples: &[f32]| {
        let settled = &samples[1_000..];
        (settled.iter().map(|sample| sample * sample).sum::<f32>() / settled.len() as f32).sqrt()
    };
    let kept = rms(&resample(&recorded_tone(1_000.0), rate));
    // 16 kHz would play a 14 kHz tone as one at 2 kHz.
    let folded = rms(&resample(&recorded_tone(14_000.0), rate));
    assert!(kept > 0.35, "{kept}");
    assert!(folded < 0.01 * kept, "{folded}");
}

#[test]
fn cues_mix_onto_every_channel_and_end_in_silence() {
    let bank = Bank {
        start: [0.5, 0.25].into(),
        stop: [0.75, 0.75, 0.75].into(),
        cancel: [].into(),
        show: [].into(),
        copied: [].into(),
    };
    let (mut asks, asked) = RingBuffer::new(VOICES);
    let mut mixer = Mixer::new(bank, asked);
    let heard = Instant::now();
    asks.push((Cue::Start, heard)).unwrap();
    asks.push((Cue::Stop, heard)).unwrap();
    let mut output = [0.0_f32; 8];
    mixer.fill(&mut output, NonZeroUsize::new(2).unwrap(), heard);
    // Overlapping cues add, and their sum stops at full scale.
    assert_eq!(output, [1.0, 1.0, 1.0, 1.0, 0.75, 0.75, 0.0, 0.0]);
    mixer.fill(&mut output, NonZeroUsize::new(2).unwrap(), heard);
    assert_eq!(output, [0.0; 8]);
}

#[test]
fn a_cue_the_device_would_play_after_its_deadline_is_skipped() {
    let bank = Bank {
        start: [0.5].into(),
        stop: [0.25].into(),
        cancel: [].into(),
        show: [].into(),
        copied: [].into(),
    };
    let (mut asks, asked) = RingBuffer::new(VOICES);
    let mut mixer = Mixer::new(bank, asked);
    let heard = Instant::now();
    asks.push((
        Cue::Start,
        heard.checked_sub(Duration::from_millis(1)).unwrap(),
    ))
    .unwrap();
    asks.push((Cue::Stop, heard)).unwrap();
    let mut output = [0.0_f32; 2];
    mixer.fill(&mut output, NonZeroUsize::new(1).unwrap(), heard);
    assert_eq!(output, [0.25, 0.0]);
}

#[test]
fn a_callback_is_heard_once_the_devices_latency_has_passed() {
    let now = Instant::now();
    let (called, playing) = (
        cpal::StreamInstant::new(1, 0),
        cpal::StreamInstant::new(1, 40_000_000),
    );
    let latency = cpal::OutputStreamTimestamp {
        callback: called,
        playback: playing,
    };
    let expected = now.checked_add(Duration::from_millis(40)).unwrap();
    assert_eq!(heard(now, latency), expected);
    // A device that reports playback before its callback is heard at once.
    let backwards = cpal::OutputStreamTimestamp {
        callback: playing,
        playback: called,
    };
    assert_eq!(heard(now, backwards), now);
}

#[test]
fn only_errors_that_stop_the_stream_break_the_device() {
    for recoverable in [
        cpal::ErrorKind::Xrun,
        cpal::ErrorKind::DeviceChanged,
        cpal::ErrorKind::RealtimeDenied,
    ] {
        assert!(!breaks(recoverable), "{recoverable:?}");
    }
    assert!(breaks(cpal::ErrorKind::DeviceNotAvailable));
}

#[test]
#[ignore = "Plays every cue on the default output device; no microphone, hook, clipboard, or injected input"]
fn every_cue_plays_on_the_default_output() {
    let opening = Instant::now();
    let mut output = Output::open().expect("a default output device that can play cues");
    println!("opened the default output in {:?}", opening.elapsed());
    for cue in [Cue::Start, Cue::Stop, Cue::Cancel, Cue::Show, Cue::Copied] {
        output.start(cue, by(Instant::now()));
        thread::sleep(Duration::from_millis(700));
        assert!(!output.is_broken(), "{cue:?}");
    }
}
