//! The samples in time order, where the screen changed across them, and the candidate
//! keyframes they offer.

use std::{ops::Range, time::Duration};

use super::{Basis, Candidate};
use crate::{
    Moment, Span,
    marks::Mark,
    recording::{Action, Word},
    signature::{Sample, Signature},
};

/// A click's keyframe shows the screen this long after release, once the app has responded.
const SETTLE: Duration = Duration::from_millis(250);
/// A transition at least this strong is a scene change.
const SCENE: f32 = 0.08;
/// Scene changes and spoken cues must differ at least this much from every chosen keyframe.
const NOVELTY: f32 = 0.03;
/// A spoken cue points at a transition starting from just before the word to a little after.
const CUE_LEAD: Duration = Duration::from_millis(300);
const CUE_REACH: Duration = Duration::from_millis(1_200);
/// Without a transition, a cue shows the screen as the word was said.
const CUE_LOOK: Duration = Duration::from_millis(150);

/// The samples in time order. Every frame index this view hands out is in bounds.
pub(super) struct Frames<'a> {
    samples: Vec<&'a Sample>,
    first: &'a Sample,
}

impl<'a> Frames<'a> {
    pub(super) fn new(samples: &'a [Sample]) -> Option<Self> {
        let mut samples: Vec<&Sample> = samples.iter().collect();
        samples.sort_by_key(|sample| sample.at);
        Some(Self {
            first: samples.first().copied()?,
            samples,
        })
    }

    fn sample(&self, frame: usize) -> &'a Sample {
        debug_assert!(frame < self.samples.len(), "frame {frame} is out of bounds");
        self.samples.get(frame).copied().unwrap_or(self.first)
    }

    pub(super) fn at(&self, frame: usize) -> Moment {
        self.sample(frame).at
    }

    pub(super) fn signature(&self, frame: usize) -> &'a Signature {
        &self.sample(frame).signature
    }

    pub(super) fn last(&self) -> usize {
        self.samples.len().saturating_sub(1)
    }

    /// Whether frame `other` differs from frame `one` by at least [`NOVELTY`].
    pub(super) fn novel(&self, one: usize, other: usize) -> bool {
        self.signature(one).transition(self.signature(other)) >= NOVELTY
    }

    /// The frame on screen closest to `moment`.
    pub(super) fn nearest(&self, moment: Moment) -> usize {
        let after = self.samples.partition_point(|sample| sample.at < moment);
        let before = after.saturating_sub(1);
        if after <= self.last()
            && self.at(after).distance(moment) < self.at(before).distance(moment)
        {
            after
        } else {
            before.min(self.last())
        }
    }

    /// The last frame captured before `moment`, or the first frame.
    fn before(&self, moment: Moment) -> usize {
        self.samples
            .partition_point(|sample| sample.at < moment)
            .saturating_sub(1)
    }

    /// The first frame captured at or after `moment`, or the last frame.
    fn from(&self, moment: Moment) -> usize {
        self.samples
            .partition_point(|sample| sample.at < moment)
            .min(self.last())
    }

    /// Runs of consecutive frames that changed past [`SCENE`], each ending on its settled view.
    pub(super) fn transitions(&self) -> Vec<Transition> {
        let mut found = Vec::new();
        let mut open: Option<Transition> = None;
        for (frame, pair) in self.samples.windows(2).enumerate() {
            let (Some(one), Some(next)) = (pair.first(), pair.last()) else {
                continue;
            };
            let strength = one.signature.transition(&next.signature);
            let settled = frame.saturating_add(1);
            if strength > SCENE {
                open = Some(match open {
                    Some(run) => Transition {
                        frame: settled,
                        span: Span::new(run.span.start(), next.at),
                        strength: run.strength.max(strength),
                    },
                    None => Transition {
                        frame: settled,
                        span: Span::new(one.at, next.at),
                        strength,
                    },
                });
            } else {
                found.extend(open.take());
            }
        }
        found.extend(open);
        found
    }

    pub(super) fn mark_candidate(&self, marks: &[Mark], group: Range<usize>) -> Option<Candidate> {
        let members = marks.get(group.clone())?;
        let span = members.iter().map(|mark| mark.span).reduce(Span::union)?;
        Some(Candidate {
            frame: self.nearest(span.end()),
            span,
            score: 100.0,
            basis: Basis::Mark(group),
        })
    }

    pub(super) fn action_candidate(&self, action: Action) -> Candidate {
        let span = action.span();
        let before = self.before(span.start());
        let after = self
            .nearest(span.end().saturating_add(SETTLE))
            .max(self.from(span.end()));
        let change = self.signature(before).change(self.signature(after));
        Candidate {
            frame: after,
            span,
            score: 30.0f32.mul_add(change, 60.0),
            basis: Basis::Click(action),
        }
    }

    pub(super) fn cue_candidate(
        &self,
        word: &Word,
        transitions: &[Transition],
    ) -> Option<Candidate> {
        let cue = Cue::of(&word.text)?;
        let said = word.span.start();
        let snapped = transitions
            .iter()
            .filter(|transition| {
                let start = transition.span.start();
                start.saturating_add(CUE_LEAD) >= said && start <= said.saturating_add(CUE_REACH)
            })
            .min_by_key(|transition| transition.span.start().distance(said));
        let (frame, score) = match snapped {
            Some(transition) => (
                transition.frame,
                transition.score().max(cue.score) + if cue.deliberate { 6.0 } else { 1.0 },
            ),
            None => (self.nearest(said.saturating_add(CUE_LOOK)), cue.score),
        };
        Some(Candidate {
            frame,
            span: Span::at(said),
            score,
            basis: Basis::Cue {
                word: cue.word,
                snapped: snapped.is_some(),
            },
        })
    }

    pub(super) fn middle_candidate(&self) -> Candidate {
        let frame = self.samples.len() / 2;
        Candidate {
            frame,
            span: Span::at(self.at(frame)),
            score: 0.0,
            basis: Basis::Middle,
        }
    }
}

/// Consecutive frames that changed, ending on the frame where the screen settled.
#[derive(Clone, Copy)]
pub(super) struct Transition {
    pub(super) frame: usize,
    span: Span,
    strength: f32,
}

impl Transition {
    fn score(self) -> f32 {
        40.0f32.mul_add(self.strength, 40.0)
    }

    pub(super) fn candidate(&self) -> Candidate {
        Candidate {
            frame: self.frame,
            span: self.span,
            score: self.score(),
            basis: Basis::SceneChange,
        }
    }
}

/// A word that points at the screen. Deliberate cues ("now", "look") outrank ordinary scene
/// changes; the rest are as often filler ("I see why", "that's odd"), so they mostly break ties.
struct Cue {
    word: &'static str,
    score: f32,
    deliberate: bool,
}

impl Cue {
    const ALL: [(&'static str, f32, bool); 10] = [
        ("now", 58.0, true),
        ("here", 56.0, true),
        ("look", 58.0, true),
        ("watch", 58.0, true),
        ("see", 44.0, false),
        ("there", 44.0, false),
        ("this", 44.0, false),
        ("these", 44.0, false),
        ("that", 42.0, false),
        ("those", 42.0, false),
    ];

    fn of(text: &str) -> Option<Self> {
        let word = text
            .trim_matches(|character: char| !character.is_alphanumeric())
            .to_lowercase();
        Self::ALL
            .into_iter()
            .find(|(cue, ..)| *cue == word)
            .map(|(word, score, deliberate)| Self {
                word,
                score,
                deliberate,
            })
    }
}
