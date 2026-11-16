use std::mem;

use super::{Env, Payload, SessionId};

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
    pub tail: P::Range,
    pub empty: bool,
}

#[cfg_attr(test, derive(Clone, Debug))]
enum Stream<P: Payload> {
    Queued(P::Live),
    Sent,
    Heard(P::Heard),
}

#[cfg_attr(test, derive(Clone, Debug))]
enum Piece<P: Payload> {
    Queued(P::Audio),
    Sent,
    Heard(P::Heard),
}

#[cfg_attr(test, derive(Clone, Debug))]
enum Early<P: Payload> {
    Segmented(Vec<Piece<P>>),
    Streaming(Stream<P>),
    Whole,
}

#[cfg_attr(test, derive(Clone, Debug))]
enum Audio<P: Payload> {
    Whole,
    Tail { range: P::Range, empty: bool },
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
            self.early = Early::Streaming(Stream::Queued(audio));
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
            || pieces.iter().any(|part| matches!(part, Piece::Queued(_)))
        {
            self.whole();
            return;
        }
        pieces.push(Piece::Queued(audio));
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
                Audio::Tail {
                    range: partition.tail,
                    empty: partition.empty,
                }
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

    pub(super) fn request(&mut self, env: &mut impl Env<P>) -> Option<Request<P>> {
        if let Early::Streaming(stream) = &mut self.early {
            if !matches!(stream, Stream::Queued(_)) {
                return None;
            }
            let Stream::Queued(audio) = mem::replace(stream, Stream::Sent) else {
                return None;
            };
            return Some(Request {
                ticket: Ticket {
                    session: self.id,
                    part: Part::Stream,
                },
                source: Source::Live(audio),
                detail: Detail::Text,
            });
        }
        if let Early::Segmented(pieces) = &mut self.early
            && let Some((index, piece)) = pieces
                .iter_mut()
                .enumerate()
                .find(|(_, piece)| !matches!(piece, Piece::Heard(_)))
        {
            if matches!(piece, Piece::Sent) {
                return None;
            }
            let number = u32::try_from(index).ok()?;
            let Piece::Queued(audio) = mem::replace(piece, Piece::Sent) else {
                return None;
            };
            return Some(Request {
                ticket: Ticket {
                    session: self.id,
                    part: Part::Piece(number),
                },
                source: Source::Audio(audio),
                detail: Detail::Text,
            });
        }
        let final_part = self
            .final_part
            .as_mut()
            .filter(|part| part.progress == Progress::Waiting)?;
        let audio = match &final_part.mode {
            Audio::Tail { empty: true, .. } => return None,
            Audio::Tail { range, .. } => {
                if let Some(tail) = env.cut(&final_part.audio, range) {
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
                session: self.id,
                part: Part::Final,
            },
            source: Source::Audio(audio),
            detail: final_part.detail,
        })
    }

    pub(super) fn accepts(&self, ticket: Ticket) -> bool {
        ticket.session == self.id
            && match ticket.part {
                Part::Stream => matches!(self.early, Early::Streaming(Stream::Sent)),
                Part::Piece(number) => match &self.early {
                    Early::Segmented(pieces) => usize::try_from(number)
                        .ok()
                        .and_then(|index| pieces.get(index))
                        .is_some_and(|part| matches!(part, Piece::Sent)),
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
        match ticket.part {
            Part::Stream => {
                let Transcript::Text(text) = transcript else {
                    self.whole();
                    return None;
                };
                self.early = Early::Streaming(Stream::Heard(text));
                self.complete().map(Transcript::Text)
            },
            Part::Piece(number) => {
                let Transcript::Text(text) = transcript else {
                    self.whole();
                    return None;
                };
                let Early::Segmented(pieces) = &mut self.early else {
                    return None;
                };
                let part = usize::try_from(number)
                    .ok()
                    .and_then(|index| pieces.get_mut(index))?;
                *part = Piece::Heard(text);
                self.complete().map(Transcript::Text)
            },
            Part::Final => {
                let final_part = self.final_part.as_mut()?;
                final_part.progress = Progress::Done;
                Some(match transcript {
                    Transcript::Text(tail) if matches!(final_part.mode, Audio::Tail { .. }) => {
                        Transcript::Text(self.join(tail))
                    },
                    transcript => transcript,
                })
            },
        }
    }

    pub(super) fn complete(&mut self) -> Option<P::Heard> {
        let final_part = self
            .final_part
            .as_mut()
            .filter(|part| part.progress == Progress::Waiting)?;
        if let Early::Streaming(Stream::Heard(text)) = &mut self.early {
            final_part.progress = Progress::Done;
            return Some(mem::take(text));
        }
        let Early::Segmented(pieces) = &self.early else {
            return None;
        };
        if !matches!(final_part.mode, Audio::Tail { empty: true, .. })
            || pieces.iter().any(|part| !matches!(part, Piece::Heard(_)))
        {
            return None;
        }
        final_part.progress = Progress::Done;
        Some(self.join(P::Heard::default()))
    }

    fn join(&mut self, tail: P::Heard) -> P::Heard {
        let mut text = P::Heard::default();
        if let Early::Segmented(pieces) = &mut self.early {
            for part in mem::take(pieces) {
                if let Piece::Heard(raw) = part {
                    P::append(&mut text, raw);
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
