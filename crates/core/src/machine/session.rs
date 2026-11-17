use std::mem;

use super::{
    Env, Lost, NarrationLoss, Payload, Phase, SessionId,
    recognition::{Detail, Partition, Recognition, Request, Ticket, Transcript},
};
use crate::gesture::State;

#[cfg_attr(test, derive(Clone, Debug))]
pub(super) struct Session<P: Payload> {
    pub(super) id: SessionId,
    pub(super) permit: P::Permit,
    pub(super) started: P::Instant,
    pub(super) purpose: Purpose<P>,
    pub(super) style: P::Style,
    pub(super) pending_text: Option<P::Heard>,
    pub(super) lost: Option<Lost<P>>,
    stage: Stage<P>,
    recognition: Recognition<P>,
    show: Option<Show<P>>,
    receipt: Option<P::Receipt>,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub(super) enum Purpose<P: Payload> {
    Dictation,
    Replay,
    Practice(P::Practice),
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Kept<P: Payload> {
    pub checkpoint: P::Checkpoint,
    pub audio: P::AudioLog,
    pub frames: P::FrameLog,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Sealed<P: Payload> {
    pub audio: P::Audio,
    pub trimmed: P::Span,
    pub partition: Option<Partition<P>>,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Reopened<P: Payload> {
    pub footage: P::Footage,
    pub checkpoint: P::Checkpoint,
    pub pending: P::Pending,
    pub recognized: Recognized<P>,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Recognized<P: Payload> {
    Speech {
        audio: P::Audio,
        start: P::Span,
        incomplete: bool,
    },
    Narration {
        narration: P::Narration,
        loss: Option<NarrationLoss>,
    },
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Origin<P: Payload> {
    Live,
    Recovered { pending: P::Pending },
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Publication<P: Payload> {
    pub footage: P::Footage,
    pub narration: P::Narration,
    pub loss: Option<NarrationLoss>,
    pub checkpoint: Option<P::Checkpoint>,
    pub origin: Origin<P>,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Release<P: Payload> {
    Keep(Option<(P::Narration, bool)>),
    Discard,
}

#[cfg_attr(test, derive(Clone, Debug))]
struct Show<P: Payload> {
    screen: Screen<P>,
    audio: Option<P::AudioLog>,
    frames: Option<P::FrameLog>,
    checkpoint: Option<P::Checkpoint>,
    disposition: Disposition,
    origin: Origin<P>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Disposition {
    Unfinished,
    Finished,
    Forfeited,
}

impl<P: Payload> Show<P> {
    const fn new(screen: Screen<P>) -> Self {
        Self {
            screen,
            audio: None,
            frames: None,
            checkpoint: None,
            disposition: Disposition::Unfinished,
            origin: Origin::Live,
        }
    }

    fn conclude(&mut self) {
        if self.disposition == Disposition::Unfinished {
            self.disposition = Disposition::Finished;
        }
    }

    fn close_producers(&mut self) {
        self.audio = None;
        self.frames = None;
    }
}

#[cfg_attr(test, derive(Clone, Debug))]
enum Screen<P: Payload> {
    Queued(P::Scope),
    Recording,
    Recorded(P::Footage),
    Spent,
}

impl<P: Payload> Screen<P> {
    fn take_footage(&mut self) -> Option<P::Footage> {
        match mem::replace(self, Self::Spent) {
            Self::Recorded(footage) => Some(footage),
            other => {
                *self = other;
                None
            },
        }
    }
}

#[cfg_attr(test, derive(Clone, Debug))]
enum Stage<P: Payload> {
    Queued,
    Opening,
    Reopening,
    Recording {
        audio: P::Span,
    },
    Stopping {
        audio: P::Span,
    },
    Recognizing {
        incomplete: bool,
    },
    AwaitingFootage {
        narration: P::Narration,
        lost: Option<NarrationLoss>,
    },
    Delivering,
}

impl<P: Payload> Session<P> {
    pub(super) const fn new(
        id: SessionId,
        permit: P::Permit,
        started: P::Instant,
        style: P::Style,
    ) -> Self {
        Self {
            id,
            permit,
            started,
            purpose: Purpose::Dictation,
            style,
            pending_text: None,
            lost: None,
            stage: Stage::Queued,
            recognition: Recognition::new(id),
            show: None,
            receipt: None,
        }
    }

    pub(super) fn recovering(
        id: SessionId,
        permit: P::Permit,
        started: P::Instant,
        style: P::Style,
        receipt: P::Receipt,
    ) -> Self {
        let mut session = Self::new(id, permit, started, style);
        session.show = Some(Show::new(Screen::Spent));
        session.receipt = Some(receipt);
        session.stage = Stage::Reopening;
        session
    }

    pub(super) fn take_receipt(&mut self) -> Option<P::Receipt> {
        self.receipt.take()
    }

    pub(super) fn reopened(&mut self, reopened: Reopened<P>) {
        let Reopened {
            footage,
            checkpoint,
            pending,
            recognized,
        } = reopened;
        let mut show = Show::new(Screen::Recorded(footage));
        show.checkpoint = Some(checkpoint);
        show.disposition = Disposition::Finished;
        show.origin = Origin::Recovered { pending };
        self.stage = match recognized {
            Recognized::Speech {
                audio,
                start,
                incomplete,
            } => {
                self.recognition.seal(audio, None, start, Detail::Words);
                Stage::Recognizing { incomplete }
            },
            Recognized::Narration { narration, loss } => Stage::AwaitingFootage {
                narration,
                lost: loss,
            },
        };
        self.show = Some(show);
    }

    pub(super) fn keep(&mut self, kept: Kept<P>) {
        if let Some(show) = &mut self.show {
            show.checkpoint = Some(kept.checkpoint);
            show.audio = Some(kept.audio);
            show.frames = Some(kept.frames);
        }
    }

    pub(super) fn take_frame_log(&mut self) -> Option<P::FrameLog> {
        self.show.as_mut()?.frames.take()
    }

    pub(super) fn take_audio_log(&mut self) -> Option<(P::AudioLog, P::Span)> {
        let (Stage::Recording { audio } | Stage::Stopping { audio }) = self.stage else {
            return None;
        };
        self.show.as_mut()?.audio.take().map(|log| (log, audio))
    }

    pub(super) fn forfeit(&mut self) {
        if let Some(show) = &mut self.show {
            show.disposition = Disposition::Forfeited;
        }
    }

    fn conclude(&mut self) {
        if let Some(show) = &mut self.show {
            show.conclude();
        }
    }

    pub(super) const fn phase(&self) -> Phase {
        match self.stage {
            Stage::Queued | Stage::Opening => Phase::Starting,
            Stage::Recording { .. } => Phase::Recording,
            Stage::Stopping { .. } => Phase::Stopping,
            Stage::Reopening
            | Stage::Recognizing { .. }
            | Stage::AwaitingFootage { .. }
            | Stage::Delivering => Phase::Processing,
        }
    }

    pub(super) const fn is_show(&self) -> bool {
        self.show.is_some()
    }

    pub(super) const fn is_practice(&self) -> bool {
        matches!(self.purpose, Purpose::Practice(_))
    }

    pub(super) const fn is_queued(&self) -> bool {
        matches!(self.stage, Stage::Queued)
    }

    pub(super) const fn is_recording(&self) -> bool {
        matches!(self.stage, Stage::Recording { .. })
    }

    pub(super) const fn is_listening(&self) -> bool {
        matches!(self.stage, Stage::Recording { .. } | Stage::Stopping { .. })
    }

    pub(super) const fn is_capturing(&self) -> bool {
        matches!(
            self.stage,
            Stage::Opening | Stage::Recording { .. } | Stage::Stopping { .. }
        )
    }

    #[cfg(test)]
    pub(super) const fn is_delivering(&self) -> bool {
        matches!(self.stage, Stage::Delivering)
    }

    #[cfg(test)]
    pub(super) const fn is_reopening(&self) -> bool {
        matches!(self.stage, Stage::Reopening)
    }

    pub(super) const fn awaits_delivery(&self) -> bool {
        matches!(self.stage, Stage::Delivering | Stage::Reopening)
    }

    #[cfg(test)]
    pub(super) const fn records_screen(&self) -> bool {
        matches!(
            self.show,
            Some(Show {
                screen: Screen::Recording,
                ..
            })
        )
    }

    pub(super) fn awaits_worker(&self) -> bool {
        self.is_recognizing() && self.recognition.waiting()
    }

    pub(super) fn is_transcribing(&self) -> bool {
        self.is_recognizing() && self.recognition.in_flight()
    }

    pub(super) const fn is_recognizing(&self) -> bool {
        matches!(self.stage, Stage::Recognizing { .. })
    }

    pub(super) const fn queued_screen(&self) -> Option<P::Scope> {
        match &self.show {
            Some(Show {
                screen: Screen::Queued(scope),
                ..
            }) => Some(*scope),
            _ => None,
        }
    }

    pub(super) fn opened(&mut self) {
        if self.is_queued() {
            self.stage = Stage::Opening;
        }
    }

    pub(super) fn audio_ready(&mut self, gesture: State, at: P::Instant) {
        if gesture == State::Processing {
            self.conclude();
        }
        if matches!(self.stage, Stage::Opening) {
            let audio = P::offset(self.started, at);
            self.stage = if gesture == State::Processing {
                Stage::Stopping { audio }
            } else {
                Stage::Recording { audio }
            };
        }
    }

    pub(super) fn finish(&mut self) {
        self.conclude();
        if let Stage::Recording { audio } = self.stage {
            self.stage = Stage::Stopping { audio };
        }
    }

    pub(super) fn captured(&mut self, sealed: Option<Sealed<P>>) -> bool {
        match sealed {
            Some(sealed) if self.is_listening() => self.recognize(sealed),
            _ if self.is_show() => {
                self.stage = Stage::AwaitingFootage {
                    narration: P::Narration::default(),
                    lost: None,
                };
            },
            _ => return false,
        }
        self.conclude();
        true
    }

    pub(super) fn lost_microphone(&mut self, sealed: Sealed<P>, lost: Lost<P>) {
        self.lost = Some(lost);
        self.recognize(sealed);
        self.conclude();
    }

    fn recognize(&mut self, sealed: Sealed<P>) {
        let (Stage::Recording { audio } | Stage::Stopping { audio }) = self.stage else {
            return;
        };
        let detail = if self.is_show() {
            Detail::Words
        } else {
            Detail::Text
        };
        let start = P::shift(audio, sealed.trimmed);
        self.recognition
            .seal(sealed.audio, sealed.partition, start, detail);
        self.stage = Stage::Recognizing { incomplete: false };
    }

    pub(super) fn piece(&mut self, number: u32, audio: P::Audio) {
        if self.is_capturing() && !self.is_show() {
            self.recognition.piece(number, audio);
        }
    }

    pub(super) fn stream(&mut self, audio: P::Live) {
        if self.is_capturing() && !self.is_show() {
            self.recognition.stream(audio);
        }
    }

    pub(super) fn request(&mut self, env: &mut impl Env<P>) -> Option<Request<P>> {
        if self.is_capturing() || self.is_recognizing() {
            self.recognition.request(env)
        } else {
            None
        }
    }

    pub(super) fn accepts(&self, ticket: Ticket) -> bool {
        (self.is_capturing() || self.is_recognizing()) && self.recognition.accepts(ticket)
    }

    pub(super) fn heard(&mut self, ticket: Ticket, transcript: Transcript<P>) -> Option<P::Heard> {
        if !self.accepts(ticket) {
            return None;
        }
        match self.recognition.heard(ticket, transcript)? {
            Transcript::Text(text) => Some(text),
            Transcript::Words(narration) => {
                self.narrate(narration);
                None
            },
        }
    }

    pub(super) fn complete(&mut self) -> Option<P::Heard> {
        if self.is_recognizing() {
            self.recognition.complete()
        } else {
            None
        }
    }

    pub(super) fn failed(&mut self, ticket: Ticket) -> bool {
        !self.accepts(ticket) || self.recognition.failed(ticket)
    }

    fn narrate(&mut self, narration: P::Narration) {
        if let Stage::Recognizing { incomplete } = self.stage
            && let Some(start) = self.recognition.start()
        {
            self.recognition = Recognition::new(self.id);
            self.stage = Stage::AwaitingFootage {
                narration: P::delay(narration, start),
                lost: (incomplete || self.lost.is_some()).then_some(NarrationLoss::Incomplete),
            };
        }
    }

    pub(super) fn audio_failed(&mut self, silent: bool) -> bool {
        let listened = self.is_show() && self.is_listening();
        if listened {
            self.go_on_without_narration(if silent {
                NarrationLoss::AudioSilent
            } else {
                NarrationLoss::AudioStopped
            });
        }
        listened
    }

    pub(super) fn not_transcribed(&mut self) -> bool {
        let given_up =
            self.is_show() && self.is_recognizing() && self.recognition.may_lose_narration();
        if given_up {
            self.go_on_without_narration(NarrationLoss::NotTranscribed);
        }
        given_up
    }

    fn go_on_without_narration(&mut self, lost: NarrationLoss) {
        self.recognition = Recognition::new(self.id);
        self.stage = Stage::AwaitingFootage {
            narration: P::Narration::default(),
            lost: Some(lost),
        };
        self.conclude();
    }

    pub(super) fn insert(&mut self, text: P::Heard, config: &P::Config, env: &mut impl Env<P>) {
        self.recognition = Recognition::new(self.id);
        env.insert(text, &self.permit, config);
        self.stage = Stage::Delivering;
    }

    pub(super) fn turn_into_show(&mut self, scope: P::Scope, env: &mut impl Env<P>) -> bool {
        if self.is_practice() || self.is_show() || !(self.is_queued() || self.is_capturing()) {
            return false;
        }
        if !env.shield(&self.permit) {
            return false;
        }
        self.recognition.whole();
        self.show = Some(Show::new(Screen::Queued(scope)));
        true
    }

    pub(super) fn screen_opened(&mut self) {
        if let Some(Show {
            screen: screen @ Screen::Queued(_),
            ..
        }) = &mut self.show
        {
            *screen = Screen::Recording;
        }
    }

    pub(super) fn screen_recorded(&mut self, footage: P::Footage) {
        if let Some(show) = &mut self.show {
            show.screen = Screen::Recorded(footage);
        }
    }

    pub(super) fn publish(&mut self, env: &mut impl Env<P>) {
        let (Stage::AwaitingFootage { narration, lost }, Some(show)) =
            (&mut self.stage, &mut self.show)
        else {
            return;
        };
        let Some(footage) = show.screen.take_footage() else {
            return;
        };
        let narration = mem::take(narration);
        let loss = *lost;
        show.close_producers();
        let origin = mem::replace(&mut show.origin, Origin::Live);
        let checkpoint = show.checkpoint.take();
        env.publish(
            Publication {
                footage,
                narration,
                loss,
                checkpoint,
                origin,
            },
            &self.permit,
            self.receipt.take(),
        );
        self.stage = Stage::Delivering;
    }

    pub(super) fn into_cleanup(self, env: &mut impl Env<P>) -> bool {
        if self.awaits_delivery() {
            env.retire_delivery(self.id);
            return true;
        }
        let Some(mut show) = self.show else {
            return false;
        };
        let release = match show.disposition {
            Disposition::Finished => Release::Keep(match self.stage {
                Stage::AwaitingFootage {
                    narration,
                    lost: lost @ (None | Some(NarrationLoss::Incomplete)),
                } => Some((narration, lost.is_some())),
                _ => None,
            }),
            Disposition::Unfinished | Disposition::Forfeited => Release::Discard,
        };
        show.close_producers();
        let Some(checkpoint) = show.checkpoint else {
            return false;
        };
        env.release(self.id, checkpoint, release);
        true
    }
}

#[cfg(not(test))]
redacted!(
    Kept,
    Sealed,
    Reopened,
    Recognized,
    Origin,
    Publication,
    Release
);

#[cfg(test)]
mod tests;
