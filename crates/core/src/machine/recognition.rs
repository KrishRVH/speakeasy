use std::mem;

use super::{Payload, SessionId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket {
    pub session: SessionId,
    pub part: Part,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Stream,
    Piece(u32),
    Final,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Detail {
    Text,
    Words,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Transcript<P: Payload> {
    Text(P::Heard),
    Words(P::Narration),
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Request<P: Payload> {
    pub ticket: Ticket,
    pub source: Source<P>,
    pub detail: Detail,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub enum Source<P: Payload> {
    Audio(P::Audio),
    Live(P::Live),
}

#[cfg_attr(test, derive(Clone, Debug))]
pub struct Partition<P: Payload> {
    pub pieces: u32,
    pub tail: Option<P::Range>,
}

#[cfg_attr(test, derive(Clone, Debug))]
enum Job<A, T> {
    Queued(A),
    Sent,
    Heard(T),
}

impl<A, T> Job<A, T> {
    fn send(&mut self) -> Option<A> {
        match mem::replace(self, Self::Sent) {
            Self::Queued(audio) => Some(audio),
            other => {
                *self = other;
                None
            },
        }
    }
}

#[cfg_attr(test, derive(Clone, Debug))]
enum Early<P: Payload> {
    Segmented(Vec<Job<P::Audio, P::Heard>>),
    Streaming(Job<P::Live, P::Heard>),
    Whole,
}

#[cfg_attr(test, derive(Clone, Debug))]
enum Audio<P: Payload> {
    Whole,
    Tail(Option<P::Range>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Progress {
    Waiting,
    Sent,
    Done,
}

#[cfg_attr(test, derive(Clone, Debug))]
struct Final<P: Payload> {
    audio: P::Audio,
    start: P::Span,
    detail: Detail,
    mode: Audio<P>,
    progress: Progress,
    retried: bool,
}

#[cfg_attr(test, derive(Clone, Debug))]
pub(super) struct Recognition<P: Payload> {
    id: SessionId,
    early: Early<P>,
    final_part: Option<Final<P>>,
}

impl<P: Payload> Recognition<P> {
    pub(super) const fn new(id: SessionId) -> Self {
        Self {
            id,
            early: Early::Segmented(Vec::new()),
            final_part: None,
        }
    }

    pub(super) fn stream(&mut self, audio: P::Live) {
        if matches!(self.early, Early::Segmented(_)) && self.final_part.is_none() {
            self.early = Early::Streaming(Job::Queued(audio));
        }
    }

    pub(super) fn piece(&mut self, number: u32, audio: P::Audio) {
        let Early::Segmented(pieces) = &mut self.early else {
            return;
        };
        if self.final_part.is_some() {
            return;
        }
        if usize::try_from(number).ok() != Some(pieces.len())
            || pieces.iter().any(|piece| matches!(piece, Job::Queued(_)))
        {
            self.whole();
            return;
        }
        pieces.push(Job::Queued(audio));
    }

    pub(super) fn whole(&mut self) {
        self.early = Early::Whole;
        if let Some(final_part) = &mut self.final_part {
            final_part.mode = Audio::Whole;
        }
    }

    pub(super) fn seal(
        &mut self,
        audio: P::Audio,
        partition: Option<Partition<P>>,
        start: P::Span,
        detail: Detail,
    ) {
        let mode = match (&self.early, detail, partition) {
            (Early::Streaming(_), Detail::Text, _) => Audio::Whole,
            (Early::Segmented(pieces), Detail::Text, Some(partition))
                if usize::try_from(partition.pieces).ok() == Some(pieces.len())
                    && partition.pieces > 0 =>
            {
                Audio::Tail(partition.tail)
            },
            _ => {
                self.whole();
                Audio::Whole
            },
        };
        self.final_part = Some(Final {
            audio,
            start,
            detail,
            mode,
            progress: Progress::Waiting,
            retried: false,
        });
    }

    pub(super) fn request(
        &mut self,
        cut: impl FnOnce(&P::Audio, &P::Range) -> Option<P::Audio>,
    ) -> Option<Request<P>> {
        let session = self.id;
        let early = |part, source| Request {
            ticket: Ticket { session, part },
            source,
            detail: Detail::Text,
        };
        if let Early::Streaming(stream) = &mut self.early {
            return stream
                .send()
                .map(|audio| early(Part::Stream, Source::Live(audio)));
        }
        if let Early::Segmented(pieces) = &mut self.early
            && let Some((index, piece)) = pieces
                .iter_mut()
                .enumerate()
                .find(|(_, piece)| !matches!(piece, Job::Heard(_)))
        {
            let number = u32::try_from(index).ok()?;
            return piece
                .send()
                .map(|audio| early(Part::Piece(number), Source::Audio(audio)));
        }
        let final_part = self
            .final_part
            .as_mut()
            .filter(|part| part.progress == Progress::Waiting)?;
        let audio = match &final_part.mode {
            Audio::Tail(None) => return None,
            Audio::Tail(Some(range)) => {
                if let Some(tail) = cut(&final_part.audio, range) {
                    tail
                } else {
                    final_part.mode = Audio::Whole;
                    self.early = Early::Whole;
                    final_part.audio.clone()
                }
            },
            Audio::Whole => final_part.audio.clone(),
        };
        final_part.progress = Progress::Sent;
        Some(Request {
            ticket: Ticket {
                session,
                part: Part::Final,
            },
            source: Source::Audio(audio),
            detail: final_part.detail,
        })
    }

    pub(super) fn accepts(&self, ticket: Ticket) -> bool {
        ticket.session == self.id
            && match ticket.part {
                Part::Stream => matches!(self.early, Early::Streaming(Job::Sent)),
                Part::Piece(number) => match &self.early {
                    Early::Segmented(pieces) => usize::try_from(number)
                        .ok()
                        .and_then(|index| pieces.get(index))
                        .is_some_and(|piece| matches!(piece, Job::Sent)),
                    Early::Streaming(_) | Early::Whole => false,
                },
                Part::Final => self.in_flight(),
            }
    }

    pub(super) fn heard(
        &mut self,
        ticket: Ticket,
        transcript: Transcript<P>,
    ) -> Option<Transcript<P>> {
        if !self.accepts(ticket) {
            return None;
        }
        match (ticket.part, transcript) {
            (Part::Final, transcript) => {
                let final_part = self.final_part.as_mut()?;
                final_part.progress = Progress::Done;
                Some(match transcript {
                    Transcript::Text(tail) if matches!(final_part.mode, Audio::Tail(_)) => {
                        Transcript::Text(self.join(tail))
                    },
                    transcript => transcript,
                })
            },
            (Part::Stream | Part::Piece(_), Transcript::Words(_)) => {
                self.whole();
                None
            },
            (Part::Stream, Transcript::Text(text)) => {
                self.early = Early::Streaming(Job::Heard(text));
                self.complete().map(Transcript::Text)
            },
            (Part::Piece(number), Transcript::Text(text)) => {
                let Early::Segmented(pieces) = &mut self.early else {
                    return None;
                };
                let piece = usize::try_from(number)
                    .ok()
                    .and_then(|index| pieces.get_mut(index))?;
                *piece = Job::Heard(text);
                self.complete().map(Transcript::Text)
            },
        }
    }

    pub(super) fn complete(&mut self) -> Option<P::Heard> {
        let final_part = self
            .final_part
            .as_mut()
            .filter(|part| part.progress == Progress::Waiting)?;
        if let Early::Streaming(Job::Heard(text)) = &mut self.early {
            final_part.progress = Progress::Done;
            return Some(mem::take(text));
        }
        let Early::Segmented(pieces) = &self.early else {
            return None;
        };
        if !matches!(final_part.mode, Audio::Tail(None))
            || pieces.iter().any(|piece| !matches!(piece, Job::Heard(_)))
        {
            return None;
        }
        final_part.progress = Progress::Done;
        Some(self.join(P::Heard::default()))
    }

    fn join(&mut self, tail: P::Heard) -> P::Heard {
        let mut text = P::Heard::default();
        if let Early::Segmented(pieces) = &mut self.early {
            for piece in mem::take(pieces) {
                if let Job::Heard(heard) = piece {
                    P::append(&mut text, heard);
                }
            }
        }
        P::append(&mut text, tail);
        text
    }

    pub(super) fn failed(&mut self, ticket: Ticket) -> bool {
        if !self.accepts(ticket) {
            return true;
        }
        self.whole();
        if matches!(ticket.part, Part::Piece(_) | Part::Stream) {
            return true;
        }
        if let Some(final_part) = &mut self.final_part
            && !final_part.retried
        {
            final_part.retried = true;
            final_part.progress = Progress::Waiting;
            return true;
        }
        false
    }

    pub(super) fn waiting(&self) -> bool {
        self.final_part
            .as_ref()
            .is_some_and(|part| part.progress == Progress::Waiting)
    }

    pub(super) fn in_flight(&self) -> bool {
        self.final_part
            .as_ref()
            .is_some_and(|part| part.progress == Progress::Sent)
    }

    pub(super) fn may_lose_narration(&self) -> bool {
        self.final_part.as_ref().is_some_and(|part| {
            part.progress == Progress::Waiting || (part.progress == Progress::Sent && part.retried)
        })
    }

    pub(super) fn start(&self) -> Option<P::Span> {
        self.final_part.as_ref().map(|part| part.start)
    }
}

#[cfg(not(test))]
redacted!(Transcript, Request, Source, Partition);

#[cfg(test)]
mod tests;
