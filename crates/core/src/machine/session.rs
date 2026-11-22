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
    purpose: Purpose<P>,
    pub(super) style: P::Style,
    stage: Stage<P>,
    recognition: Recognition<P>,
    receipt: Option<P::Receipt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Enter {
    Withheld,
    Pressed,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Handoff<P: Payload> {
    Insert(Enter),
    Capture,
    Print(P::Call),
}

#[cfg_attr(test, derive(Clone, Debug))]
pub(super) enum Purpose<P: Payload> {
    Dictation(Handoff<P>),
    Replay,
    Practice(P::Practice),
    Show(Show<P>),
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
    Live {
        checkpoint: Option<P::Checkpoint>,
    },
    Recovered {
        checkpoint: P::Checkpoint,
        pending: P::Pending,
    },
}

impl<P: Payload> Origin<P> {
    fn into_checkpoint(self) -> Option<P::Checkpoint> {
        match self {
            Self::Live { checkpoint } => checkpoint,
            Self::Recovered { checkpoint, .. } => Some(checkpoint),
        }
    }
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Publication<P: Payload> {
    pub footage: P::Footage,
    pub narration: P::Narration,
    pub loss: Option<NarrationLoss>,
    pub origin: Origin<P>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    Keep,
    Discard,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub(super) struct Show<P: Payload> {
    screen: Screen<P>,
    audio: Option<P::AudioLog>,
    frames: Option<P::FrameLog>,
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
            disposition: Disposition::Unfinished,
            origin: Origin::Live { checkpoint: None },
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
        start: P::Span,
    },
    Stopping {
        start: P::Span,
    },
    Recognizing(Ended<P>),
    AwaitingFootage {
        narration: P::Narration,
        loss: Option<NarrationLoss>,
    },
    Delivering {
        dictation: Option<P::Heard>,
    },
}

#[cfg_attr(test, derive(Clone, Debug))]
enum Ended<P: Payload> {
    Finished,
    Early,
    Lost(Lost<P>),
}

impl<P: Payload> Ended<P> {
    const fn narration_loss(&self) -> Option<NarrationLoss> {
        match self {
            Self::Finished => None,
            Self::Early | Self::Lost(_) => Some(NarrationLoss::Incomplete),
        }
    }

    fn take_lost(&mut self) -> Option<Lost<P>> {
        match mem::replace(self, Self::Early) {
            Self::Lost(lost) => Some(lost),
            other => {
                *self = other;
                None
            },
        }
    }
}

impl<P: Payload> Session<P> {
    pub(super) const fn new(
        id: SessionId,
        permit: P::Permit,
        started: P::Instant,
        purpose: Purpose<P>,
        style: P::Style,
    ) -> Self {
        Self {
            id,
            permit,
            started,
            purpose,
            style,
            stage: Stage::Queued,
            recognition: Recognition::new(id),
            receipt: None,
        }
    }

    pub(super) fn recovering(
        id: SessionId,
        permit: P::Permit,
        started: P::Instant,
        receipt: P::Receipt,
    ) -> Self {
        Self {
            stage: Stage::Reopening,
            receipt: Some(receipt),
            ..Self::new(
                id,
                permit,
                started,
                Purpose::Show(Show::new(Screen::Spent)),
                P::Style::default(),
            )
        }
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
        self.stage = match recognized {
            Recognized::Speech {
                audio,
                start,
                incomplete,
            } => {
                self.recognition.seal(audio, None, start, Detail::Words);
                Stage::Recognizing(if incomplete {
                    Ended::Early
                } else {
                    Ended::Finished
                })
            },
            Recognized::Narration { narration, loss } => Stage::AwaitingFootage { narration, loss },
        };
        self.purpose = Purpose::Show(Show {
            disposition: Disposition::Finished,
            origin: Origin::Recovered {
                checkpoint,
                pending,
            },
            ..Show::new(Screen::Recorded(footage))
        });
    }

    const fn show_mut(&mut self) -> Option<&mut Show<P>> {
        match &mut self.purpose {
            Purpose::Show(show) => Some(show),
            Purpose::Dictation(_) | Purpose::Replay | Purpose::Practice(_) => None,
        }
    }

    pub(super) fn keep(&mut self, kept: Kept<P>) {
        if let Some(Show {
            origin: Origin::Live { checkpoint },
            audio,
            frames,
            ..
        }) = self.show_mut()
        {
            *checkpoint = Some(kept.checkpoint);
            *audio = Some(kept.audio);
            *frames = Some(kept.frames);
        }
    }

    pub(super) fn take_frame_log(&mut self) -> Option<P::FrameLog> {
        self.show_mut()?.frames.take()
    }

    pub(super) fn take_audio_log(&mut self) -> Option<(P::AudioLog, P::Span)> {
        let (Stage::Recording { start } | Stage::Stopping { start }) = self.stage else {
            return None;
        };
        self.show_mut()?.audio.take().map(|log| (log, start))
    }

    pub(super) fn forfeit(&mut self) {
        if let Some(show) = self.show_mut() {
            show.disposition = Disposition::Forfeited;
        }
    }

    fn conclude(&mut self) {
        if let Some(show) = self.show_mut()
            && show.disposition == Disposition::Unfinished
        {
            show.disposition = Disposition::Finished;
        }
    }

    pub(super) const fn phase(&self) -> Phase {
        match self.stage {
            Stage::Queued | Stage::Opening => Phase::Starting,
            Stage::Recording { .. } => Phase::Recording,
            Stage::Stopping { .. } => Phase::Stopping,
            Stage::Reopening
            | Stage::Recognizing(_)
            | Stage::AwaitingFootage { .. }
            | Stage::Delivering { .. } => Phase::Processing,
        }
    }

    pub(super) const fn is_dictation(&self) -> bool {
        matches!(self.purpose, Purpose::Dictation(_))
    }

    pub(super) const fn is_replay(&self) -> bool {
        matches!(self.purpose, Purpose::Replay)
    }

    pub(super) const fn is_show(&self) -> bool {
        matches!(self.purpose, Purpose::Show(_))
    }

    pub(super) const fn is_practice(&self) -> bool {
        matches!(self.purpose, Purpose::Practice(_))
    }

    pub(super) const fn practice(&self) -> Option<&P::Practice> {
        match &self.purpose {
            Purpose::Practice(practice) => Some(practice),
            Purpose::Dictation(_) | Purpose::Replay | Purpose::Show(_) => None,
        }
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
        matches!(self.stage, Stage::Delivering { .. })
    }

    #[cfg(test)]
    pub(super) const fn is_reopening(&self) -> bool {
        matches!(self.stage, Stage::Reopening)
    }

    pub(super) const fn awaits_delivery(&self) -> bool {
        matches!(self.stage, Stage::Delivering { .. } | Stage::Reopening)
    }

    #[cfg(test)]
    pub(super) const fn records_screen(&self) -> bool {
        matches!(
            self.purpose,
            Purpose::Show(Show {
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
        matches!(self.stage, Stage::Recognizing(_))
    }

    #[cfg(test)]
    pub(super) const fn lost_its_microphone(&self) -> bool {
        matches!(self.stage, Stage::Recognizing(Ended::Lost(_)))
    }

    pub(super) fn take_lost(&mut self) -> Option<Lost<P>> {
        let Stage::Recognizing(ended) = &mut self.stage else {
            return None;
        };
        ended.take_lost()
    }

    pub(super) fn take_dictation(&mut self) -> Option<P::Heard> {
        let Stage::Delivering { dictation } = &mut self.stage else {
            return None;
        };
        dictation.take()
    }

    pub(super) const fn queued_screen(&self) -> Option<P::Scope> {
        match &self.purpose {
            Purpose::Show(Show {
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
            let start = P::offset(self.started, at);
            self.stage = if gesture == State::Processing {
                Stage::Stopping { start }
            } else {
                Stage::Recording { start }
            };
        }
    }

    pub(super) fn finish(&mut self) {
        self.conclude();
        if let Stage::Recording { start } = self.stage {
            self.stage = Stage::Stopping { start };
        }
    }

    pub(super) fn captured(&mut self, sealed: Option<Sealed<P>>) -> bool {
        match sealed {
            Some(sealed) if self.is_listening() => self.recognize(sealed, Ended::Finished),
            _ if self.is_show() => {
                self.stage = Stage::AwaitingFootage {
                    narration: P::Narration::default(),
                    loss: None,
                };
            },
            _ => return false,
        }
        self.conclude();
        true
    }

    pub(super) fn lost_microphone(&mut self, sealed: Sealed<P>, lost: Lost<P>) {
        self.recognize(sealed, Ended::Lost(lost));
        self.conclude();
    }

    fn recognize(&mut self, sealed: Sealed<P>, ended: Ended<P>) {
        let (Stage::Recording { start } | Stage::Stopping { start }) = self.stage else {
            return;
        };
        let detail = if self.is_show() {
            Detail::Words
        } else {
            Detail::Text
        };
        let start = P::shift(start, sealed.trimmed);
        self.recognition
            .seal(sealed.audio, sealed.partition, start, detail);
        self.stage = Stage::Recognizing(ended);
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
            self.recognition
                .request(|audio, range| env.cut(audio, range))
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

    pub(super) fn absorb_failure(&mut self, ticket: Ticket) -> bool {
        !self.accepts(ticket) || self.recognition.absorb_failure(ticket)
    }

    fn narrate(&mut self, narration: P::Narration) {
        if let Stage::Recognizing(ended) = &self.stage
            && let Some(start) = self.recognition.start()
        {
            let loss = ended.narration_loss();
            self.recognition = Recognition::new(self.id);
            self.stage = Stage::AwaitingFootage {
                narration: P::delay(narration, start),
                loss,
            };
        }
    }

    pub(super) fn go_on_without_audio(&mut self, silent: bool) -> bool {
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

    pub(super) fn go_on_without_transcript(&mut self) -> bool {
        let given_up =
            self.is_show() && self.is_recognizing() && self.recognition.may_lose_narration();
        if given_up {
            self.go_on_without_narration(NarrationLoss::NotTranscribed);
        }
        given_up
    }

    fn go_on_without_narration(&mut self, loss: NarrationLoss) {
        self.recognition = Recognition::new(self.id);
        self.stage = Stage::AwaitingFootage {
            narration: P::Narration::default(),
            loss: Some(loss),
        };
        self.conclude();
    }

    pub(super) fn submit(&mut self) {
        if let Purpose::Dictation(Handoff::Insert(enter)) = &mut self.purpose {
            *enter = Enter::Pressed;
        }
    }

    pub(super) const fn for_script(&self) -> bool {
        matches!(
            self.purpose,
            Purpose::Dictation(Handoff::Capture | Handoff::Print(_))
        )
    }

    pub(super) fn capture(&mut self) {
        if let Purpose::Dictation(handoff) = &mut self.purpose {
            *handoff = Handoff::Capture;
        }
    }

    pub(super) fn print_to(&mut self, call: P::Call) -> Result<(), P::Call> {
        match &mut self.purpose {
            Purpose::Dictation(handoff @ Handoff::Capture) => {
                *handoff = Handoff::Print(call);
                Ok(())
            },
            Purpose::Dictation(Handoff::Insert(_) | Handoff::Print(_))
            | Purpose::Replay
            | Purpose::Practice(_)
            | Purpose::Show(_) => Err(call),
        }
    }

    pub(super) fn take_caller(&mut self) -> Option<P::Call> {
        let Purpose::Dictation(handoff) = &mut self.purpose else {
            return None;
        };
        match mem::replace(handoff, Handoff::Capture) {
            Handoff::Print(call) => Some(call),
            other => {
                *handoff = other;
                None
            },
        }
    }

    pub(super) fn insert(&mut self, text: P::Heard, config: &P::Config, env: &mut impl Env<P>) {
        let (handoff, dictation) = match &mut self.purpose {
            Purpose::Dictation(handoff) => (
                mem::replace(handoff, Handoff::Insert(Enter::Withheld)),
                Some(text.clone()),
            ),
            Purpose::Replay | Purpose::Practice(_) | Purpose::Show(_) => {
                (Handoff::Insert(Enter::Withheld), None)
            },
        };
        self.recognition = Recognition::new(self.id);
        env.insert(text, &self.permit, config, handoff);
        self.stage = Stage::Delivering { dictation };
    }

    pub(super) fn turn_into_show(&mut self, scope: P::Scope, env: &mut impl Env<P>) -> bool {
        if !self.is_dictation()
            || self.for_script()
            || !(self.is_queued() || self.is_capturing())
            || !env.shield(&self.permit)
        {
            return false;
        }
        self.recognition.whole();
        self.purpose = Purpose::Show(Show::new(Screen::Queued(scope)));
        true
    }

    pub(super) fn screen_opened(&mut self) {
        if let Some(Show {
            screen: screen @ Screen::Queued(_),
            ..
        }) = self.show_mut()
        {
            *screen = Screen::Recording;
        }
    }

    pub(super) fn screen_recorded(&mut self, footage: P::Footage) {
        if let Some(show) = self.show_mut() {
            show.screen = Screen::Recorded(footage);
        }
    }

    pub(super) fn publish(&mut self, env: &mut impl Env<P>) {
        let (Stage::AwaitingFootage { narration, loss }, Purpose::Show(show)) =
            (&mut self.stage, &mut self.purpose)
        else {
            return;
        };
        let Some(footage) = show.screen.take_footage() else {
            return;
        };
        let narration = mem::take(narration);
        let loss = *loss;
        show.close_producers();
        let origin = mem::replace(&mut show.origin, Origin::Live { checkpoint: None });
        env.publish(
            Publication {
                footage,
                narration,
                loss,
                origin,
            },
            &self.permit,
            self.receipt.take(),
        );
        self.stage = Stage::Delivering { dictation: None };
    }

    pub(super) fn into_cleanup(self, env: &mut impl Env<P>) -> bool {
        if self.awaits_delivery() {
            env.retire_delivery(self.id);
            return true;
        }
        let Purpose::Show(mut show) = self.purpose else {
            return false;
        };
        let release = match show.disposition {
            Disposition::Finished => Release::Keep,
            Disposition::Unfinished | Disposition::Forfeited => Release::Discard,
        };
        show.close_producers();
        let Some(checkpoint) = show.origin.into_checkpoint() else {
            return false;
        };
        env.release(self.id, checkpoint, release);
        true
    }
}

#[cfg(not(test))]
redacted!(
    Kept,
    Handoff,
    Sealed,
    Reopened,
    Recognized,
    Origin,
    Publication
);

#[cfg(test)]
mod tests;
