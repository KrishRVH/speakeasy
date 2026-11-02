use std::{ops::Range, time::Duration};

use super::{
    pcm::{bytes_in, samples_in},
    speech::Audibility,
};
use crate::runtime::ports::Partition;

const MINIMUM_PIECE: Duration = Duration::from_secs(20);

pub(super) struct Cuts {
    minimum: usize,
    end: Option<usize>,
    pieces: u32,
}

impl Cuts {
    pub(super) fn new(rate: u32) -> Self {
        Self {
            minimum: bytes_in(samples_in(MINIMUM_PIECE, rate)),
            end: None,
            pieces: 0,
        }
    }

    pub(super) fn offer(&self, speech: &Audibility, length: usize) -> Option<Range<usize>> {
        let kept = speech.paused(length)?;
        let start = self.end.unwrap_or(kept.start);
        (kept.end.saturating_sub(start) >= self.minimum).then_some(start..kept.end)
    }

    pub(super) const fn number(&self) -> u32 {
        self.pieces
    }

    pub(super) fn sent(&mut self, end: usize) {
        self.end = Some(end);
        self.pieces = self.pieces.saturating_add(1);
    }

    pub(super) fn finish(&self, speech: &Audibility, length: usize) -> Option<Partition> {
        let end = self.end?;
        let kept = speech.retained(length)?;
        if end > kept.end {
            return None;
        }
        Some(Partition {
            pieces: self.pieces,
            tail: end.checked_sub(kept.start)?..kept.len(),
        })
    }
}

#[cfg(test)]
mod tests;
