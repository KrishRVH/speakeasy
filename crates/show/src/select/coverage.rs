//! Splitting stretches of narration that one keyframe would own for too long.

use std::time::Duration;

use super::Chooser;
use crate::{Moment, Span, recording::Word};

/// A keyframe owns too much narration past this many words or this long.
const COVERAGE_WORDS: f32 = 100.0;
const COVERAGE_TIME: Duration = Duration::from_secs(35);

impl Chooser<'_> {
    /// Splits stretches of narration a keyframe owns that run past [`COVERAGE_WORDS`] or
    /// [`COVERAGE_TIME`], with a frame from inside them where the screen changed.
    ///
    /// The best-ranked candidate that leaves no part above three quarters of the stretch wins,
    /// else the most even split. The first keyframe also owns the lead-in before it, but a frame
    /// added there must split the lead-in evenly too, so it lands mid-gap rather than beside the
    /// first keyframe.
    pub(super) fn cover_long_stretches(&mut self, words: &[Word]) {
        let coverage = Coverage::new(words, self.frames.at(self.frames.last()));
        if self.chosen.is_empty() {
            // The best-ranked candidate goes first, as it would without coverage.
            if let Some(first) = self.pending.iter().position(Option::is_some) {
                self.take(first);
            }
        }
        let mut rejected = vec![false; self.pending.len()];
        while !self.full(usize::MAX) {
            let mut anchors: Vec<Moment> = self
                .chosen
                .iter()
                .map(|candidate| candidate.anchor(self.frames))
                .collect();
            anchors.sort_unstable();
            let Some(pick) = self.split_longest(&coverage, &anchors, &mut rejected) else {
                break;
            };
            self.take(pick);
        }
    }

    fn split_longest(
        &self,
        coverage: &Coverage,
        anchors: &[Moment],
        rejected: &mut [bool],
    ) -> Option<usize> {
        let mut long: Vec<(f32, Span)> = coverage
            .stretches(anchors, false)
            .into_iter()
            .map(|stretch| (coverage.size(stretch), stretch))
            .filter(|(size, _)| *size > 1.0)
            .collect();
        long.sort_by(|one, other| other.0.total_cmp(&one.0));
        for (whole, stretch) in long {
            let mut options: Vec<Split> = self
                .pending
                .iter()
                .enumerate()
                .filter(|(index, _)| !rejected.get(*index).copied().unwrap_or(true))
                .filter_map(|(index, slot)| {
                    let anchor = slot.as_ref()?.anchor(self.frames);
                    coverage.split(anchors, stretch, whole, anchor, index)
                })
                .collect();
            options.sort_by(Split::cmp_preference);
            for option in options {
                if self.pending_admitted(option.index) {
                    return Some(option.index);
                }
                if let Some(flag) = rejected.get_mut(option.index) {
                    *flag = true;
                }
            }
        }
        None
    }
}

/// How much narration and time each stretch between keyframes holds.
struct Coverage {
    /// Spoken word onsets, in order.
    onsets: Vec<Moment>,
    end: Moment,
}

/// A candidate that would split a long stretch, ranked by how evenly it splits it.
struct Split {
    uneven: bool,
    worst: f32,
    worst_seen: f32,
    index: usize,
}

impl Split {
    /// Even splits first in candidate rank order; uneven ones by how much they still leave.
    fn cmp_preference(one: &Self, other: &Self) -> std::cmp::Ordering {
        one.uneven.cmp(&other.uneven).then_with(|| {
            if one.uneven {
                one.worst
                    .total_cmp(&other.worst)
                    .then(one.worst_seen.total_cmp(&other.worst_seen))
            } else {
                one.index.cmp(&other.index)
            }
        })
    }
}

impl Coverage {
    fn new(words: &[Word], last_frame: Moment) -> Self {
        let mut onsets: Vec<Moment> = words
            .iter()
            .filter(|word| word.is_spoken())
            .map(|word| word.span.start())
            .collect();
        onsets.sort_unstable();
        let end = onsets
            .last()
            .map_or(last_frame, |&last| last.max(last_frame))
            .saturating_add(Duration::from_millis(1));
        Self { onsets, end }
    }

    /// How far past the limits a stretch runs: above one is too long.
    fn size(&self, stretch: Span) -> f32 {
        let words = self
            .onsets
            .partition_point(|onset| *onset < stretch.end())
            .saturating_sub(
                self.onsets
                    .partition_point(|onset| *onset < stretch.start()),
            );
        (words as f32 / COVERAGE_WORDS).max(stretch.duration().div_duration_f32(COVERAGE_TIME))
    }

    /// The stretches between sorted `anchors`. Each keyframe owns from its anchor to the next;
    /// the first also owns the lead-in, unless `seen` asks for what each frame showed, which
    /// keeps the lead-in apart.
    fn stretches(&self, anchors: &[Moment], seen: bool) -> Vec<Span> {
        let bounds: Vec<Moment> = std::iter::once(Moment::START)
            .chain(anchors.iter().skip(usize::from(!seen)).copied())
            .chain([self.end])
            .collect();
        bounds
            .iter()
            .zip(bounds.iter().skip(1))
            .map(|(&start, &end)| Span::new(start, end))
            .collect()
    }

    fn worst_within(&self, pieces: Vec<Span>, bounds: Span) -> f32 {
        pieces
            .into_iter()
            .filter(|piece| bounds.contains(piece.start()) && bounds.contains(piece.end()))
            .map(|piece| self.size(piece))
            .fold(0.0, f32::max)
    }

    /// How adding a keyframe anchored at `anchor` would split `stretch`, if it shrinks it.
    fn split(
        &self,
        anchors: &[Moment],
        stretch: Span,
        whole: f32,
        anchor: Moment,
        index: usize,
    ) -> Option<Split> {
        let Err(position) = anchors.binary_search(&anchor) else {
            return None;
        };
        if anchor < stretch.start() || anchor >= stretch.end() {
            return None;
        }
        let mut with = anchors.to_vec();
        with.insert(position, anchor);
        let worst = self.worst_within(self.stretches(&with, false), stretch);
        if worst >= whole {
            return None;
        }
        let gap = self
            .stretches(anchors, true)
            .into_iter()
            .find(|gap| gap.start() <= anchor && anchor < gap.end())
            .unwrap_or(stretch);
        let worst_seen = self.worst_within(self.stretches(&with, true), gap);
        Some(Split {
            uneven: worst > 0.75 * whole || worst_seen > 0.75 * self.size(gap),
            worst,
            worst_seen,
            index,
        })
    }
}
