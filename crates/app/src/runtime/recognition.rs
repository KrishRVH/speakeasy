use std::{mem, ops::Range, time::Duration};

use speakeasy_show::Narration;
use zeroize::Zeroizing;

use super::ports::{LiveAudio, Sealed, SessionId};
use crate::audio::wav_tail;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Ticket {
    pub session: SessionId,
    pub part: Part,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Part {
    Stream,
    Piece(u32),
    Final,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Detail {
    Text,
    Words,
}

pub(super) enum Transcript {
    Text(String),
    Words(Narration),
}

pub(super) struct Request {
    pub ticket: Ticket,
    pub audio: Source,
    pub detail: Detail,
    pub language: Option<Box<str>>,
}

pub(super) enum Source {
    Wav(Sealed),
    Stream(LiveAudio),
}

enum Stream {
    Queued(LiveAudio),
    Sent,
    Heard(Zeroizing<String>),
}

enum Piece {
    Queued(Sealed),
    Sent,
    Heard(Zeroizing<String>),
}

enum Early {
    Segmented(Vec<Piece>),
    Streaming(Stream),
    Whole,
}

enum Audio {
    Whole,
    Tail(Range<usize>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Sent,
    Done,
}

struct Final {
    audio: Sealed,
    start: Duration,
    language: Option<Box<str>>,
    detail: Detail,
    mode: Audio,
    phase: Phase,
    retried: bool,
}

pub(super) struct Recognition {
    id: SessionId,
    early: Early,
    final_part: Option<Final>,
}

impl Recognition {
    pub(super) const fn new(id: SessionId) -> Self {
        Self {
            id,
            early: Early::Segmented(Vec::new()),
            final_part: None,
        }
    }

    pub(super) fn stream(&mut self, audio: LiveAudio) {
        if matches!(self.early, Early::Segmented(_)) && self.final_part.is_none() {
            self.early = Early::Streaming(Stream::Queued(audio));
        }
    }

    pub(super) fn piece(&mut self, number: u32, audio: Sealed) {
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
        mut audio: Sealed,
        start: Duration,
        language: Option<Box<str>>,
        detail: Detail,
    ) {
        let mode = match (&self.early, detail, audio.partition.take()) {
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
            language,
            detail,
            mode,
            phase: Phase::Waiting,
            retried: false,
        });
    }

    pub(super) fn request(&mut self) -> Option<Request> {
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
                audio: Source::Stream(audio),
                detail: Detail::Text,
                language: None,
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
                audio: Source::Wav(audio),
                detail: Detail::Text,
                language: None,
            });
        }
        let final_part = self
            .final_part
            .as_mut()
            .filter(|part| part.phase == Phase::Waiting)?;
        let wav = match &final_part.mode {
            Audio::Tail(range) if range.is_empty() => return None,
            Audio::Tail(range) => {
                if let Ok(wav) = wav_tail(&final_part.audio.wav, range.clone()) {
                    wav
                } else {
                    final_part.mode = Audio::Whole;
                    self.early = Early::Whole;
                    final_part.audio.wav.clone()
                }
            },
            Audio::Whole => final_part.audio.wav.clone(),
        };
        final_part.phase = Phase::Sent;
        Some(Request {
            ticket: Ticket {
                session: self.id,
                part: Part::Final,
            },
            audio: Source::Wav(Sealed {
                wav,
                trimmed: Duration::ZERO,
                partition: None,
            }),
            detail: final_part.detail,
            language: final_part.language.clone(),
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

    pub(super) fn heard(&mut self, ticket: Ticket, transcript: Transcript) -> Option<Transcript> {
        if !self.accepts(ticket) {
            return None;
        }
        match ticket.part {
            Part::Stream => {
                let Transcript::Text(text) = transcript else {
                    self.whole();
                    return None;
                };
                self.early = Early::Streaming(Stream::Heard(Zeroizing::new(text)));
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
                *part = Piece::Heard(Zeroizing::new(text));
                self.complete().map(Transcript::Text)
            },
            Part::Final => {
                let final_part = self.final_part.as_mut()?;
                final_part.phase = Phase::Done;
                Some(match transcript {
                    Transcript::Text(tail) if matches!(final_part.mode, Audio::Tail(_)) => {
                        Transcript::Text(self.join(&tail))
                    },
                    transcript => transcript,
                })
            },
        }
    }

    pub(super) fn complete(&mut self) -> Option<String> {
        let final_part = self
            .final_part
            .as_mut()
            .filter(|part| part.phase == Phase::Waiting)?;
        if let Early::Streaming(Stream::Heard(text)) = &mut self.early {
            final_part.phase = Phase::Done;
            return Some(mem::take(&mut **text));
        }
        let Early::Segmented(pieces) = &self.early else {
            return None;
        };
        if !matches!(&final_part.mode, Audio::Tail(range) if range.is_empty())
            || pieces.iter().any(|part| !matches!(part, Piece::Heard(_)))
        {
            return None;
        }
        final_part.phase = Phase::Done;
        Some(self.join(""))
    }

    fn join(&mut self, tail: &str) -> String {
        let mut text = String::new();
        if let Early::Segmented(pieces) = &mut self.early {
            for part in mem::take(pieces) {
                if let Piece::Heard(raw) = part {
                    append(&mut text, &raw);
                }
            }
        }
        append(&mut text, tail);
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
            final_part.phase = Phase::Waiting;
            return true;
        }
        false
    }

    pub(super) fn waiting(&self) -> bool {
        self.final_part
            .as_ref()
            .is_some_and(|part| part.phase == Phase::Waiting)
    }

    pub(super) fn in_flight(&self) -> bool {
        self.final_part
            .as_ref()
            .is_some_and(|part| part.phase == Phase::Sent)
    }

    pub(super) fn may_lose_narration(&self) -> bool {
        self.final_part.as_ref().is_some_and(|part| {
            part.phase == Phase::Waiting || (part.phase == Phase::Sent && part.retried)
        })
    }

    pub(super) fn start(&self) -> Option<Duration> {
        self.final_part.as_ref().map(|part| part.start)
    }
}

fn append(text: &mut String, next: &str) {
    let next = next.trim();
    if !text.is_empty() && !next.is_empty() {
        text.push(' ');
    }
    text.push_str(next);
}

#[cfg(test)]
mod tests;
