//! Choosing the frames that tell a Show's story.

mod coverage;
mod frames;
mod groups;

use std::{ops::Range, time::Duration};

use serde::{Deserialize, Serialize};

use self::{
    frames::{Frames, Transition},
    groups::group_marks,
};
use crate::{
    Moment, Span, align, describe,
    marks::{self, Mark},
    recording::{Action, Recording},
    signature::Sample,
};

/// Frames are sampled this often: fast enough to catch what a click changed, slow enough that a
/// still screen costs nothing and a busy one stays cheap.
///
/// How long a click's result settles and how far apart keyframes keep are tuned against it.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// Keyframes other than marks keep at least this far apart.
const FRAME_GAP: Duration = Duration::from_millis(750);
/// Clicks, scene changes, and cues are added only while fewer keyframes than this are chosen;
/// beyond it, only marks and long stretches of narration earn one.
const ORDINARY: usize = 6;
/// Clicks this close in time and place are one click.
const DOUBLE_CLICK: Duration = Duration::from_millis(650);
const DOUBLE_CLICK_DISTANCE: f32 = 0.03;
/// The most keyframes any Show holds.
const MOST_KEYFRAMES: usize = 12;

/// Why a frame became a keyframe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    /// Ink was drawn on it.
    Mark,
    /// It shows the result of a click or drag.
    Click,
    /// The screen settled after changing.
    SceneChange,
    /// A word such as "look" or "now" pointed at it.
    Cue,
    /// Nothing else stood out, so it shows the screen midway through.
    Middle,
}

/// A frame chosen to show, with what happened on it and what was said.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Keyframe {
    /// The keyframe's number, from one, as badged on the image and named in the bundle.
    pub index: usize,
    /// When the frame was on screen.
    pub at: Moment,
    /// Why it was chosen.
    pub reason: Reason,
    /// What happened, for a reader.
    pub event: String,
    /// The narration that belongs to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    /// The marks drawn on it; empty unless the reason is [`Reason::Mark`].
    #[serde(with = "crate::serde_range")]
    pub marks: Range<usize>,
    /// The click or drag whose result it shows; on a mark's keyframe, the last one made while its
    /// ink was drawn that no keyframe of its own shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
}

/// The marks a Show's ink forms and the keyframes chosen to show it.
///
/// Only [`select`] makes one, so it holds at least one keyframe, numbered from one in time order,
/// and its keyframes refer only to its own marks.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub(crate) marks: Vec<Mark>,
    pub(crate) keyframes: Vec<Keyframe>,
}

impl Selection {
    /// The chosen frames, in time order.
    #[must_use]
    pub fn keyframes(&self) -> &[Keyframe] {
        &self.keyframes
    }

    /// When each chosen frame was on screen, in keyframe order.
    #[must_use]
    pub fn moments(&self) -> Vec<Moment> {
        self.keyframes.iter().map(|keyframe| keyframe.at).collect()
    }
}

/// Chooses the keyframes that tell `recording`'s story from `samples`, which may arrive in any
/// order.
///
/// A Show shorter than 40 seconds holds up to six keyframes, and one more for each further 20
/// seconds, up to twelve. Each keyframe costs a reading model about a thousand tokens, so a longer
/// Show earns more only as it tells more.
///
/// Marks come first: every group of ink drawn on one screen gets a keyframe, merging the most
/// alike screens until they fit. Clicks, scene changes, and spoken cues such as "look here" fill
/// up to six, and long stretches of narration without a keyframe earn one from inside them.
/// While ink is drawn, another keyframe is chosen only where the screen differs from the mark's,
/// and the mark's keyframe reports the last click or drag made meanwhile that has no keyframe of
/// its own. A cue never repeats a screen already chosen, so talking over a still screen yields one
/// frame.
///
/// `None` only when there are no samples to choose from.
#[must_use]
pub fn select(recording: &Recording, samples: &[Sample]) -> Option<Selection> {
    select_within(
        recording,
        samples,
        budget(recording.duration.since(Moment::START)),
    )
}

/// The keyframes a Show of this length may hold, as [`select`] describes.
fn budget(duration: Duration) -> usize {
    const BASE: usize = 6;
    let extra = duration.saturating_sub(Duration::from_secs(20)).as_secs() / 20;
    BASE.saturating_add(usize::try_from(extra).unwrap_or(MOST_KEYFRAMES))
        .min(MOST_KEYFRAMES)
}

/// [`select`] with up to `budget` keyframes, never more than twelve; `None` also when `budget` is
/// zero.
fn select_within(recording: &Recording, samples: &[Sample], budget: usize) -> Option<Selection> {
    let budget = budget.min(MOST_KEYFRAMES);
    let frames = Frames::new(samples).filter(|_| budget > 0)?;
    let marks = marks::group(&recording.strokes, recording.surface);
    let actions = settle(&recording.actions);
    let action_times: Vec<Moment> = actions.iter().map(|action| action.span().start()).collect();
    let transitions = frames.transitions();
    let mut candidates: Vec<Candidate> = group_marks(&frames, &marks, &action_times, budget)
        .into_iter()
        .filter_map(|group| frames.mark_candidate(&marks, group))
        .collect();
    candidates.extend(
        actions
            .iter()
            .map(|&action| frames.action_candidate(action)),
    );
    candidates.extend(transitions.iter().map(Transition::candidate));
    candidates.extend(
        recording
            .narration
            .words()
            .iter()
            .filter_map(|word| frames.cue_candidate(word, &transitions)),
    );
    if candidates.is_empty() {
        candidates.push(frames.middle_candidate());
    }
    candidates.sort_by(|one, other| one.rank(&frames).cmp_rank(&other.rank(&frames)));

    let mut chooser = Chooser {
        frames: &frames,
        pending: candidates.into_iter().map(Some).collect(),
        chosen: Vec::new(),
        budget,
    };
    chooser.choose_marks();
    chooser.cover_long_stretches(recording.narration.words());
    chooser.fill_ordinary();
    chooser.drop_repeated_cues();
    let mut chosen = chooser.chosen;
    chosen.sort_by_key(|candidate| frames.at(candidate.frame));
    fold_actions_into_marks(&mut chosen, &actions);

    let anchors: Vec<Moment> = chosen
        .iter()
        .map(|candidate| candidate.anchor(&frames))
        .collect();
    let keyframes: Vec<Keyframe> = chosen
        .into_iter()
        .zip(align::align(&anchors, &recording.narration))
        .enumerate()
        .map(|(position, (candidate, said))| Keyframe {
            index: position.saturating_add(1),
            at: frames.at(candidate.frame),
            reason: candidate.basis.reason(),
            event: describe::event(&candidate.basis, &marks),
            said,
            marks: candidate.basis.marks(),
            action: candidate.basis.action(),
        })
        .collect();
    (!keyframes.is_empty()).then_some(Selection { marks, keyframes })
}

struct Candidate {
    frame: usize,
    span: Span,
    score: f32,
    basis: Basis,
}

/// What a candidate frame shows, which decides how it ranks and what its keyframe says.
#[derive(Clone, Debug)]
pub(crate) enum Basis {
    /// The selection's marks in this range, drawn on one screen, and the last click or drag made
    /// while they were drawn that no keyframe of its own shows, known once keyframes are chosen.
    Mark {
        group: Range<usize>,
        during: Option<Action>,
    },
    /// The result of a click or drag.
    Click(Action),
    /// The view the screen settled on after changing.
    SceneChange,
    /// A spoken cue, and whether it found the transition it pointed at.
    Cue { word: &'static str, snapped: bool },
    /// The screen midway through, when nothing else stood out.
    Middle,
}

impl Basis {
    const fn reason(&self) -> Reason {
        match self {
            Self::Mark { .. } => Reason::Mark,
            Self::Click(_) => Reason::Click,
            Self::SceneChange => Reason::SceneChange,
            Self::Cue { .. } => Reason::Cue,
            Self::Middle => Reason::Middle,
        }
    }

    fn marks(&self) -> Range<usize> {
        match self {
            Self::Mark { group, .. } => group.clone(),
            Self::Click(_) | Self::SceneChange | Self::Cue { .. } | Self::Middle => 0..0,
        }
    }

    const fn action(&self) -> Option<Action> {
        match *self {
            Self::Click(action) => Some(action),
            Self::Mark { during, .. } => during,
            Self::SceneChange | Self::Cue { .. } | Self::Middle => None,
        }
    }
}

/// Orders candidates: marks first, cues that found no transition last, then by score and time.
struct Rank {
    ordinary: bool,
    unsnapped_cue: bool,
    score: f32,
    at: Moment,
}

impl Rank {
    fn cmp_rank(&self, other: &Self) -> std::cmp::Ordering {
        self.ordinary
            .cmp(&other.ordinary)
            .then(self.unsnapped_cue.cmp(&other.unsnapped_cue))
            .then(other.score.total_cmp(&self.score))
            .then(self.at.cmp(&other.at))
    }
}

impl Candidate {
    fn rank(&self, frames: &Frames<'_>) -> Rank {
        Rank {
            ordinary: !matches!(self.basis, Basis::Mark { .. }),
            unsnapped_cue: matches!(self.basis, Basis::Cue { snapped: false, .. }),
            score: self.score,
            at: frames.at(self.frame),
        }
    }

    /// Where the keyframe's story begins: when its action started or its cue was said.
    fn anchor(&self, frames: &Frames<'_>) -> Moment {
        self.span.start().min(frames.at(self.frame))
    }
}

struct Chooser<'a> {
    frames: &'a Frames<'a>,
    /// Candidates in rank order; a chosen one leaves its slot empty.
    pending: Vec<Option<Candidate>>,
    chosen: Vec<Candidate>,
    budget: usize,
}

impl Chooser<'_> {
    /// Whether `candidate` adds something the chosen keyframes do not already show.
    fn admits(&self, candidate: &Candidate) -> bool {
        if matches!(candidate.basis, Basis::Mark { .. }) {
            return true;
        }
        let at = self.frames.at(candidate.frame);
        // A mark's keyframe stands for the screen its ink was drawn on, and reports a click made
        // meanwhile that has no keyframe of its own.
        let crowded = self.chosen.iter().any(|other| {
            self.frames.at(other.frame).distance(at) < FRAME_GAP
                || (matches!(other.basis, Basis::Mark { .. })
                    && other.span.contains(at)
                    && !self.frames.novel(other.frame, candidate.frame))
        });
        if crowded {
            return false;
        }
        !matches!(candidate.basis, Basis::Cue { .. } | Basis::SceneChange)
            || self
                .chosen
                .iter()
                .all(|other| self.frames.novel(other.frame, candidate.frame))
    }

    fn full(&self, limit: usize) -> bool {
        self.chosen.len() >= limit.min(self.budget)
    }

    fn choose_marks(&mut self) {
        for slot in &mut self.pending {
            if self.chosen.len() >= self.budget
                || slot
                    .as_ref()
                    .is_none_or(|c| !matches!(c.basis, Basis::Mark { .. }))
            {
                break;
            }
            self.chosen.extend(slot.take());
        }
    }

    fn fill_ordinary(&mut self) {
        for index in 0..self.pending.len() {
            if self.full(ORDINARY) {
                break;
            }
            if self.pending_admitted(index) {
                self.take(index);
            }
        }
    }

    fn pending_admitted(&self, index: usize) -> bool {
        self.pending
            .get(index)
            .and_then(Option::as_ref)
            .is_some_and(|candidate| self.admits(candidate))
    }

    fn take(&mut self, index: usize) {
        if let Some(candidate) = self.pending.get_mut(index).and_then(Option::take) {
            self.chosen.push(candidate);
        }
    }

    /// Drops a spoken cue whose screen another kind of keyframe already shows.
    fn drop_repeated_cues(&mut self) {
        let shown: Vec<usize> = self
            .chosen
            .iter()
            .filter(|candidate| !matches!(candidate.basis, Basis::Cue { .. }))
            .map(|candidate| candidate.frame)
            .collect();
        self.chosen.retain(|candidate| {
            !matches!(candidate.basis, Basis::Cue { .. })
                || shown
                    .iter()
                    .all(|&other| self.frames.novel(other, candidate.frame))
        });
    }
}

/// Gives each chosen mark the last click or drag started while its ink was drawn that has no
/// keyframe of its own.
fn fold_actions_into_marks(chosen: &mut [Candidate], actions: &[Action]) {
    let shown: Vec<Action> = chosen
        .iter()
        .filter_map(|candidate| match candidate.basis {
            Basis::Click(action) => Some(action),
            _ => None,
        })
        .collect();
    for candidate in chosen {
        let span = candidate.span;
        if let Basis::Mark { during, .. } = &mut candidate.basis {
            *during = actions
                .iter()
                .rev()
                .find(|action| span.contains(action.span().start()) && !shown.contains(action))
                .copied();
        }
    }
}

/// Actions in time order with double clicks merged into their last click.
fn settle(actions: &[Action]) -> Vec<Action> {
    let mut sorted = actions.to_vec();
    sorted.sort_by_key(|action| action.span().start());
    let mut settled: Vec<Action> = Vec::with_capacity(sorted.len());
    for action in sorted {
        if let Action::Click { at, point } = action
            && let Some(repeat) = settled.iter().rposition(|prior| {
                matches!(*prior, Action::Click { at: earlier, point: there }
                    if at.since(earlier) <= DOUBLE_CLICK
                        && point.distance(there) <= DOUBLE_CLICK_DISTANCE)
            })
        {
            settled.remove(repeat);
        }
        settled.push(action);
    }
    settled
}

#[cfg(test)]
mod tests;
