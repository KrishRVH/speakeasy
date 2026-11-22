use std::{ops::Range, sync::mpsc, time::Duration};

use super::{
    Cue, Env, Failure, Handoff, Keeping, Payload, Publication, RecognitionMode, Recorder, Refusal,
    Release, Request, SessionId, Settings, Status,
};

#[derive(Clone, Debug)]
pub(super) struct Sample;

impl Payload for Sample {
    type Audio = Vec<u8>;
    type AudioLog = mpsc::Sender<()>;
    type Call = ();
    type Checkpoint = &'static str;
    type Config = Settings;
    type Footage = &'static str;
    type FrameLog = mpsc::Sender<()>;
    type Heard = String;
    type Instant = Duration;
    type Interrupted = ();
    type Level = ();
    type Live = ();
    type Narration = Vec<Duration>;
    type Pending = bool;
    type Permit = u64;
    type Practice = ();
    type Range = Range<usize>;
    type Receipt = ();
    type Recovered = ();
    type Saved = ();
    type Scope = ();
    type Shown = &'static str;
    type Span = Duration;
    type Style = ();
    type Taps = ();
    type Text = String;
    type Unchosen = ();

    fn settings(config: &Settings) -> Settings {
        *config
    }

    fn reloads(current: &Settings, next: &Settings) -> bool {
        current.recognition != next.recognition
    }

    fn offset(started: Duration, at: Duration) -> Duration {
        at.saturating_sub(started)
    }

    fn shift(span: Duration, by: Duration) -> Duration {
        span.saturating_add(by)
    }

    fn delay(narration: Vec<Duration>, by: Duration) -> Vec<Duration> {
        narration
            .into_iter()
            .map(|word| word.saturating_add(by))
            .collect()
    }

    fn append(text: &mut String, next: String) {
        let next = next.trim();
        if !text.is_empty() && !next.is_empty() {
            text.push(' ');
        }
        text.push_str(next);
    }
}

pub(super) fn cut(audio: &[u8], range: &Range<usize>) -> Option<Vec<u8>> {
    audio.get(range.clone()).map(<[u8]>::to_vec)
}

#[derive(Debug)]
pub(super) struct Handover {
    pub publication: Publication<Sample>,
    pub producers_closed: bool,
}

#[derive(Debug, Default)]
pub(super) struct Log {
    producers: Vec<mpsc::Receiver<()>>,
    pub handover: Option<Handover>,
    pub released: Option<Release>,
}

impl Log {
    pub(super) fn producer(&mut self) -> mpsc::Sender<()> {
        let (producer, closed) = mpsc::channel();
        self.producers.push(closed);
        producer
    }
}

impl Env<Sample> for Log {
    fn admit(&mut self) -> Option<u64> {
        Some(1)
    }

    fn style(&mut self) {}

    fn shield(&mut self, _: &u64) -> bool {
        true
    }

    fn revoke(&mut self, _: &u64) {}

    fn keep(&mut self, _: Duration) -> Keeping<Sample> {
        Keeping::Unneeded
    }

    fn record_microphone(
        &mut self,
        _: SessionId,
        _: &Settings,
        _: RecognitionMode,
    ) -> Result<(), String> {
        Ok(())
    }

    fn record_screen(
        &mut self,
        _: SessionId,
        _: Duration,
        (): (),
        _: Option<mpsc::Sender<()>>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn finish(&mut self, _: Recorder) {}

    fn retire(&mut self, _: Recorder) {}

    fn listen_whole(&mut self) {}

    fn expect_cue(&mut self) {}

    fn keep_audio(&mut self, _: mpsc::Sender<()>, _: Duration, _: &Settings) -> bool {
        true
    }

    fn pin(&mut self, (): ()) {}

    fn meter(&mut self, _: SessionId, (): ()) {}

    fn load(&mut self, _: &Settings) {}

    fn reload(&mut self, _: &Settings) {}

    fn recover(&mut self, _: &Settings, _: bool) {}

    fn cut(&mut self, audio: &Vec<u8>, range: &Range<usize>) -> Option<Vec<u8>> {
        cut(audio, range)
    }

    fn transcribe(&mut self, _: Request<Sample>, _: &Settings) {}

    fn compose(&mut self, heard: String, (): (), _: &Settings) -> Option<String> {
        (!heard.is_empty()).then_some(heard)
    }

    fn insert(&mut self, _: String, _: &u64, _: &Settings, _: Handoff<Sample>) {}

    fn ready(&mut self, (): &()) -> bool {
        true
    }

    fn refuse_practice(&mut self, (): ()) {}

    fn practise(&mut self, (): &(), _: &u64, _: String) -> bool {
        true
    }

    fn publish(&mut self, publication: Publication<Sample>, _: &u64, _: Option<()>) {
        let producers_closed = self
            .producers
            .iter()
            .all(|producer| producer.try_recv() == Err(mpsc::TryRecvError::Disconnected));
        self.handover = Some(Handover {
            publication,
            producers_closed,
        });
    }

    fn reopen(&mut self, (): ()) {}

    fn refuse_recovery(&mut self, (): (), _: &Failure<Sample>) {}

    fn retire_delivery(&mut self, _: SessionId) {}

    fn release(&mut self, _: SessionId, _: &'static str, release: Release) {
        self.released = Some(release);
    }

    fn remember(&mut self, _: &'static str) {}

    fn announce(&mut self) {}

    fn cue(&mut self, _: Cue) {}

    fn hold_cues(&mut self, _: bool) {}

    fn respond(&mut self, (): (), _: Result<Status<Sample>, Refusal>) {}

    fn close_input(&mut self) {}
}
