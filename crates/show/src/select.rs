mod frames;
mod groups;

use std::{
    cmp::{Ordering, Reverse},
    fmt,
    ops::Range,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use self::{
    frames::{Frames, Response, Transition},
    groups::group_marks,
};
use crate::{
    Moment, Rect, Span, align, describe,
    framing::Plan,
    marks::{self, Mark},
    recording::{Action, Recording},
    signature::Sample,
};

pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

const FRAME_GAP: Duration = Duration::from_millis(750);

const DOUBLE_CLICK: Duration = Duration::from_millis(650);
const DOUBLE_CLICK_DISTANCE: f32 = 0.03;

pub(crate) const MOST_KEYFRAMES: usize = 36;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    Mark,
    Click,
    SceneChange,
    Cue,
    Middle,
    Opening,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Keyframe {
    pub index: usize,
    pub at: Moment,
    pub reason: Reason,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    #[serde(with = "crate::serde_range")]
    pub marks: Range<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Rect>,
}

impl Keyframe {
    #[must_use]
    pub fn marker(&self, marks: &[Mark]) -> String {
        describe::Marker(self, self.marks_in(marks)).to_string()
    }

    pub(crate) fn marks_in<'a>(&self, marks: &'a [Mark]) -> &'a [Mark] {
        marks.get(self.marks.clone()).unwrap_or_default()
    }
}

impl fmt::Debug for Keyframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            index,
            at,
            reason,
            event: _,
            said: _,
            marks,
            action,
            crop,
        } = self;
        f.debug_struct("Keyframe")
            .field("index", index)
            .field("at", at)
            .field("reason", reason)
            .field("marks", marks)
            .field("action", action)
            .field("crop", crop)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub(crate) marks: Vec<Mark>,
    pub(crate) keyframes: Vec<Keyframe>,
    pub(crate) plans: Vec<Option<Plan>>,
}

#[must_use]
pub fn select(recording: &Recording, samples: &[Sample]) -> Option<Selection> {
    select_within(recording, samples, MOST_KEYFRAMES)
}

fn select_within(recording: &Recording, samples: &[Sample], most: usize) -> Option<Selection> {
    if most == 0 {
        return None;
    }
    let frames = Frames::new(samples)?;
    let marks = marks::group(&recording.strokes, &recording.pins, recording.surface);
    let actions = merge_double_clicks(&recording.actions);
    let action_starts: Vec<Moment> = actions.iter().map(|action| action.span().start()).collect();
    let transitions = frames.transitions();
    let responses = frames.responses(&actions, &transitions);
    let mut candidates: Vec<Candidate> = group_marks(&frames, &marks, &action_starts, most)
        .into_iter()
        .filter_map(|group| frames.mark_candidate(&marks, group))
        .collect();

    candidates.extend(
        responses
            .iter()
            .map(|&response| frames.action_candidate(response)),
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

    let mut chosen = choose(candidates, &frames);
    chosen.sort_by_key(|candidate| frames.at(candidate.frame));
    if let Some(first) = chosen.first() {
        let anchor = first.anchor(&frames);
        let opening = frames.opening_candidate(anchor);
        if frames.novel(opening.frame, first.frame)
            && align::said_before(anchor, &recording.narration)
        {
            chosen.insert(0, opening);
        }
    }
    if let Some(last) = chosen.last()
        && frames.novel(last.frame, frames.last())
    {
        chosen.push(frames.last_candidate());
    }
    thin(&mut chosen, &frames, most);
    fold_actions_into_marks(&mut chosen, &responses, &frames);

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
            crop: None,
        })
        .collect();
    (!keyframes.is_empty()).then_some(Selection {
        marks,
        keyframes,
        plans: Vec::new(),
    })
}

struct Candidate {
    frame: usize,
    span: Span,
    score: f32,
    basis: Basis,
}

pub(crate) enum Basis {
    Mark {
        group: Range<usize>,
        during: Option<Action>,
    },
    Click(Action),
    SceneChange,
    Cue {
        word: &'static str,
        snapped: bool,
    },
    Middle,
    Opening,
}

impl Basis {
    const fn reason(&self) -> Reason {
        match self {
            Self::Mark { .. } => Reason::Mark,
            Self::Click(_) => Reason::Click,
            Self::SceneChange => Reason::SceneChange,
            Self::Cue { .. } => Reason::Cue,
            Self::Middle => Reason::Middle,
            Self::Opening => Reason::Opening,
        }
    }

    fn marks(&self) -> Range<usize> {
        match self {
            Self::Mark { group, .. } => group.clone(),
            Self::Click(_)
            | Self::SceneChange
            | Self::Cue { .. }
            | Self::Middle
            | Self::Opening => 0..0,
        }
    }

    const fn action(&self) -> Option<Action> {
        match *self {
            Self::Click(action) => Some(action),
            Self::Mark { during, .. } => during,
            Self::SceneChange | Self::Cue { .. } | Self::Middle | Self::Opening => None,
        }
    }

    const fn tier(&self) -> Tier {
        match self {
            Self::Mark { .. } => Tier::Mark,
            Self::Click(_)
            | Self::SceneChange
            | Self::Cue { snapped: true, .. }
            | Self::Middle
            | Self::Opening => Tier::Ordinary,
            Self::Cue { snapped: false, .. } => Tier::UnsnappedCue,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Mark,
    Ordinary,
    UnsnappedCue,
}

impl Candidate {
    fn cmp_rank(&self, other: &Self, frames: &Frames<'_>) -> Ordering {
        self.basis
            .tier()
            .cmp(&other.basis.tier())
            .then(other.score.total_cmp(&self.score))
            .then(frames.at(self.frame).cmp(&frames.at(other.frame)))
    }

    fn anchor(&self, frames: &Frames<'_>) -> Moment {
        self.span.start().min(frames.at(self.frame))
    }

    fn covers(&self, frames: &Frames<'_>, candidate: &Self) -> bool {
        let same_screen = || !frames.novel(self.frame, candidate.frame);
        match (&self.basis, &candidate.basis) {
            (Basis::Mark { .. }, _) => self.during(frames, candidate.frame) && same_screen(),
            (Basis::Click(_), Basis::Click(_)) => {
                self.frame == candidate.frame
                    || (self.span.end().distance(candidate.span.end()) < FRAME_GAP && same_screen())
            },
            (
                Basis::Click(_)
                | Basis::SceneChange
                | Basis::Cue { .. }
                | Basis::Middle
                | Basis::Opening,
                _,
            ) => self.near(frames, candidate.frame),
        }
    }

    fn near(&self, frames: &Frames<'_>, frame: usize) -> bool {
        frames.at(self.frame).distance(frames.at(frame)) < FRAME_GAP
    }

    fn during(&self, frames: &Frames<'_>, frame: usize) -> bool {
        self.near(frames, frame) || self.span.contains(frames.at(frame))
    }
}

fn choose(mut candidates: Vec<Candidate>, frames: &Frames<'_>) -> Vec<Candidate> {
    candidates.sort_by(|one, other| one.cmp_rank(other, frames));
    let mut chosen: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        if admits(&chosen, &candidate, frames) {
            chosen.push(candidate);
        }
    }
    let shown: Vec<usize> = chosen
        .iter()
        .filter(|candidate| !matches!(candidate.basis, Basis::Cue { .. }))
        .map(|candidate| candidate.frame)
        .collect();
    chosen.retain(|candidate| {
        !matches!(candidate.basis, Basis::Cue { .. })
            || shown
                .iter()
                .all(|&other| frames.novel(other, candidate.frame))
    });
    chosen
}

fn admits(chosen: &[Candidate], candidate: &Candidate, frames: &Frames<'_>) -> bool {
    let covered = || chosen.iter().any(|other| other.covers(frames, candidate));
    let novel = || {
        chosen
            .iter()
            .all(|other| frames.novel(other.frame, candidate.frame))
    };
    match candidate.basis {
        Basis::Mark { .. } => true,
        Basis::Click(_) | Basis::Middle | Basis::Opening => !covered(),
        Basis::SceneChange | Basis::Cue { .. } => !covered() && novel(),
    }
}

fn thin(chosen: &mut Vec<Candidate>, frames: &Frames<'_>, most: usize) {
    if chosen.len() <= most {
        return;
    }
    let mut slots: Vec<Slot> = chosen
        .iter()
        .enumerate()
        .map(|(index, candidate)| Slot {
            at: frames.at(candidate.frame),
            new_screen: index
                .checked_sub(1)
                .and_then(|previous| chosen.get(previous))
                .is_none_or(|previous| frames.novel(previous.frame, candidate.frame)),
            kept: matches!(candidate.basis, Basis::Mark { .. }),
        })
        .collect();
    let mut room = most.saturating_sub(slots.iter().filter(|slot| slot.kept).count());
    if room > 0
        && let Some(last) = slots.last_mut()
        && !last.kept
    {
        last.kept = true;
        room = room.saturating_sub(1);
    }
    let end = frames.at(frames.last());
    while room > 0
        && let Some(slot) = widest(&slots, end).and_then(|index| slots.get_mut(index))
    {
        slot.kept = true;
        room = room.saturating_sub(1);
    }
    let mut kept = slots.into_iter().map(|slot| slot.kept);
    chosen.retain(|_| kept.next().unwrap_or(false));
}

struct Slot {
    at: Moment,
    new_screen: bool,
    kept: bool,
}

fn widest(slots: &[Slot], end: Moment) -> Option<usize> {
    let kept: Vec<Moment> = slots
        .iter()
        .filter(|slot| slot.kept)
        .map(|slot| slot.at)
        .collect();
    slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| !slot.kept)
        .min_by_key(|(_, slot)| {
            let (before, after) = kept.split_at(kept.partition_point(|&at| at < slot.at));
            let previous = before.last().copied().unwrap_or(Moment::START);
            let next = after.first().copied().unwrap_or(end);
            let imbalance = slot.at.since(previous).abs_diff(next.since(slot.at));
            (Reverse(next.since(previous)), !slot.new_screen, imbalance)
        })
        .map(|(index, _)| index)
}

fn fold_actions_into_marks(chosen: &mut [Candidate], responses: &[Response], frames: &Frames<'_>) {
    for &Response { action, frame, .. } in responses {
        let shown = chosen
            .iter()
            .any(|candidate| matches!(candidate.basis, Basis::Click(own) if own == action));
        if shown {
            continue;
        }
        let nearest = chosen
            .iter_mut()
            .filter(|candidate| {
                matches!(candidate.basis, Basis::Mark { .. }) && candidate.during(frames, frame)
            })
            .min_by_key(|candidate| frames.at(candidate.frame).distance(frames.at(frame)));
        if let Some(Basis::Mark { during, .. }) = nearest.map(|mark| &mut mark.basis) {
            *during = Some(action);
        }
    }
}

fn merge_double_clicks(actions: &[Action]) -> Vec<Action> {
    let mut sorted = actions.to_vec();
    sorted.sort_by_key(|action| action.span().start());
    let mut merged: Vec<Action> = Vec::with_capacity(sorted.len());
    for action in sorted {
        if let Some(repeat) = merged
            .iter()
            .rposition(|&prior| is_double_click(prior, action))
        {
            merged.remove(repeat);
        }
        merged.push(action);
    }
    merged
}

fn is_double_click(earlier: Action, later: Action) -> bool {
    matches!(
        (earlier, later),
        (Action::Click { at: first, point: here }, Action::Click { at: second, point: there })
            if second.since(first) <= DOUBLE_CLICK && here.distance(there) <= DOUBLE_CLICK_DISTANCE
    )
}

#[cfg(test)]
mod tests;
