//! The samples in time order, where the screen changed across them, and the candidate
//! keyframes they offer.

use std::{ops::Range, time::Duration};

use super::{Basis, Candidate};
use crate::{
    Moment, Span,
    marks::Mark,
    recording::{Action, Word},
    signature::{SCENE, Sample, Signature},
};

/// A click that started no transition shows the screen this long after release, once the app
/// has responded.
const SETTLE: Duration = Duration::from_millis(250);
/// Scene changes and spoken cues must differ at least this much from every chosen keyframe, and
/// any keyframe during or near a mark, or a click close to another, from that keyframe.
const NOVELTY: f32 = 0.03;
/// A transition that a click started or a spoken cue points at starts at most this long before
/// it, since a transition starts on the last frame captured before the screen changed.
const LEAD: Duration = Duration::from_millis(300);
/// An app starts answering a click or drag within this long of its release. A change that starts
/// later is something a Show does not record, such as a scroll or a key press.
const CLICK_REACH: Duration = Duration::from_millis(500);
/// A spoken cue points at a transition starting up to this long after the word.
const CUE_REACH: Duration = Duration::from_millis(1_200);
/// A screen still changing this long after a click or cue is playing, as a video or a long scroll
/// does, so where it settles shows something later than what was pointed at.
const SETTLE_LIMIT: Duration = Duration::from_secs(2);
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

    /// The frame on screen closest to `moment`, the earlier on a tie.
    pub(super) fn nearest(&self, moment: Moment) -> usize {
        let (before, after) = (self.last_before(moment), self.first_from(moment));
        if self.at(after).distance(moment) < self.at(before).distance(moment) {
            after
        } else {
            before
        }
    }

    /// The last frame captured before `moment`, or the first frame.
    fn last_before(&self, moment: Moment) -> usize {
        self.samples
            .partition_point(|sample| sample.at < moment)
            .saturating_sub(1)
    }

    /// The first frame captured at or after `moment`, or the last frame.
    fn first_from(&self, moment: Moment) -> usize {
        self.samples
            .partition_point(|sample| sample.at < moment)
            .min(self.last())
    }

    /// Runs of consecutive frames that changed past [`SCENE`], each ending on its settled view.
    pub(super) fn transitions(&self) -> Vec<Transition> {
        let mut found = Vec::new();
        let mut open: Option<Transition> = None;
        for (frame, [one, next]) in self.samples.array_windows().enumerate() {
            let strength = one.signature.transition(&next.signature);
            if strength > SCENE {
                let (start, strongest) = open.map_or((one.at, strength), |run| {
                    (run.span.start(), run.strength.max(strength))
                });
                open = Some(Transition {
                    frame: frame.saturating_add(1),
                    span: Span::new(start, next.at),
                    strength: strongest,
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
            basis: Basis::Mark {
                group,
                during: None,
            },
        })
    }

    /// What each of `actions`, in time order, led to.
    ///
    /// An action's result is where the first transition it could have started settled: one that
    /// starts from [`LEAD`] before the action to [`CLICK_REACH`] after its release, and settles
    /// after the release, within [`SETTLE_LIMIT`] of it, and before the next action begins.
    /// Without one, it is the screen [`SETTLE`] after the release.
    pub(super) fn responses(
        &self,
        actions: &[Action],
        transitions: &[Transition],
    ) -> Vec<Response> {
        let next_starts = actions
            .iter()
            .skip(1)
            .map(|next| Some(next.span().start()))
            .chain([None]);
        actions
            .iter()
            .zip(next_starts)
            .map(|(&action, next_start)| {
                let span = action.span();
                let released = span.end();
                let started = transitions.iter().copied().find(|transition| {
                    let (start, settled) = (transition.span.start(), transition.span.end());
                    start.saturating_add(LEAD) >= span.start()
                        && start <= released.saturating_add(CLICK_REACH)
                        && settled > released
                        && settled <= released.saturating_add(SETTLE_LIMIT)
                        && next_start.is_none_or(|next| settled < next)
                });
                let frame = started.map_or_else(
                    || {
                        self.nearest(released.saturating_add(SETTLE))
                            .max(self.first_from(released))
                    },
                    |transition| transition.frame,
                );
                Response {
                    action,
                    frame,
                    started,
                }
            })
            .collect()
    }

    /// The candidate showing `response`'s result, scored at least as high as the transition the
    /// action started.
    pub(super) fn action_candidate(&self, response: Response) -> Candidate {
        let span = response.action.span();
        let before = self.last_before(span.start());
        let change = self
            .signature(before)
            .change(self.signature(response.frame));
        Candidate {
            frame: response.frame,
            span,
            score: response
                .started
                .map_or(0.0, Transition::score)
                .max(60.0 + 30.0 * change),
            basis: Basis::Click(response.action),
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
                start.saturating_add(LEAD) >= said
                    && start <= said.saturating_add(CUE_REACH)
                    && transition.span.end() <= said.saturating_add(SETTLE_LIMIT)
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

    /// The recording's last screen, shown as a scene change.
    pub(super) fn last_candidate(&self) -> Candidate {
        let frame = self.last();
        Candidate {
            frame,
            span: Span::at(self.at(frame)),
            score: 0.0,
            basis: Basis::SceneChange,
        }
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

/// A click or drag and the frame that shows its result.
#[derive(Clone, Copy)]
pub(super) struct Response {
    pub(super) action: Action,
    pub(super) frame: usize,
    /// The transition the action started, which `frame` shows settled.
    started: Option<Transition>,
}

/// Consecutive frames that changed, ending on the frame where the screen settled.
#[derive(Clone, Copy)]
pub(super) struct Transition {
    frame: usize,
    span: Span,
    strength: f32,
}

impl Transition {
    fn score(self) -> f32 {
        40.0 + 40.0 * self.strength
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
