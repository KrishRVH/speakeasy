//! Choosing the frames that tell a Show's story.

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
/// Keyframes other than marks keep at least this far apart, and from a mark's keyframe unless the
/// screen has changed since it.
const FRAME_GAP: Duration = Duration::from_millis(750);
/// Clicks this close in time and place are one click.
const DOUBLE_CLICK: Duration = Duration::from_millis(650);
const DOUBLE_CLICK_DISTANCE: f32 = 0.03;
/// The most keyframes any Show holds: six full contact sheets. Each keyframe costs a reading
/// model about a thousand tokens.
const MOST_KEYFRAMES: usize = 36;

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
#[derive(Clone, PartialEq, Serialize, Deserialize)]
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
    /// The click or drag it reports. A click's keyframe shows that action's result. A mark's
    /// keyframe reports the last action without a keyframe of its own whose result came while its
    /// ink was drawn, or close to its frame, even if that result is another screen; an action two
    /// marks qualify for goes to the nearer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
}

/// Leaves out what was said, which is the user's speech, and the event, which can quote a spoken
/// cue.
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
        } = self;
        f.debug_struct("Keyframe")
            .field("index", index)
            .field("at", at)
            .field("reason", reason)
            .field("marks", marks)
            .field("action", action)
            .finish_non_exhaustive()
    }
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

/// Chooses the keyframes that tell `recording`'s story from `samples`, which may arrive in any
/// order, each at its own moment.
///
/// Marks come first: every group of ink drawn on one screen gets a keyframe. Clicks, scene
/// changes, and spoken cues such as "look here" add keyframes anywhere in the recording, and its
/// last screen gets one unless the latest keyframe already shows it. While ink is drawn and just
/// after, another keyframe is chosen only where the screen differs from the mark's, and the
/// nearest mark's keyframe reports the last click or drag then without a keyframe of its own. A
/// cue never repeats a screen already chosen, so talking over a still screen yields one frame.
///
/// A Show holds at most 36 keyframes. Past that, the most alike neighboring marks merge until
/// they fit, the last screen keeps its keyframe while marks leave it room, and the other keyframes
/// that stay each split the longest stretch without one, so they spread across the recording.
///
/// `None` only when there are no samples to choose from.
#[must_use]
pub fn select(recording: &Recording, samples: &[Sample]) -> Option<Selection> {
    select_within(recording, samples, MOST_KEYFRAMES)
}

/// [`select`] with at most `most` keyframes; `None` also when `most` is zero.
fn select_within(recording: &Recording, samples: &[Sample], most: usize) -> Option<Selection> {
    if most == 0 {
        return None;
    }
    let frames = Frames::new(samples)?;
    let marks = marks::group(&recording.strokes, &recording.pins, recording.surface);
    let actions = merge_double_clicks(&recording.actions);
    let action_times: Vec<Moment> = actions.iter().map(|action| action.span().start()).collect();
    let transitions = frames.transitions();
    let mut candidates: Vec<Candidate> = group_marks(&frames, &marks, &action_times, most)
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
    candidates.sort_by(|one, other| one.cmp_rank(other, &frames));

    let mut chosen = choose(candidates, &frames);
    chosen.sort_by_key(|candidate| frames.at(candidate.frame));
    if let Some(last) = chosen.last()
        && frames.novel(last.frame, frames.last())
    {
        chosen.push(frames.last_candidate());
    }
    thin(&mut chosen, &frames, most);
    fold_actions_into_marks(&mut chosen, &actions, &frames);

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
    /// When what the frame shows was under way: its ink, action, screen change, or cue.
    span: Span,
    score: f32,
    basis: Basis,
}

/// What a candidate frame shows, which decides how it ranks and what its keyframe says.
pub(crate) enum Basis {
    /// The selection's marks in this range, and the last click or drag without a keyframe of its
    /// own whose result came while they were drawn or close to the frame, known once keyframes are
    /// chosen.
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

    const fn tier(&self) -> Tier {
        match self {
            Self::Mark { .. } => Tier::Mark,
            Self::Click(_) | Self::SceneChange | Self::Cue { snapped: true, .. } | Self::Middle => {
                Tier::Ordinary
            },
            Self::Cue { snapped: false, .. } => Tier::UnsnappedCue,
        }
    }
}

/// Where a candidate ranks before its score: marks first, cues that found no transition last.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Mark,
    Ordinary,
    UnsnappedCue,
}

impl Candidate {
    /// Orders candidates for choosing: by tier, then the higher score, then the earlier frame.
    fn cmp_rank(&self, other: &Self, frames: &Frames<'_>) -> Ordering {
        self.basis
            .tier()
            .cmp(&other.basis.tier())
            .then(other.score.total_cmp(&self.score))
            .then(frames.at(self.frame).cmp(&frames.at(other.frame)))
    }

    /// Where the keyframe's story begins: the start of its span, or its frame if that is earlier.
    fn anchor(&self, frames: &Frames<'_>) -> Moment {
        self.span.start().min(frames.at(self.frame))
    }

    /// Whether this chosen keyframe already stands for `frame`. Another keyframe too close in time
    /// does. A mark does while its ink was being drawn or close to its frame, unless the screen has
    /// changed since, so the screen a click opens right after a drawing still gets a keyframe.
    fn covers(&self, frames: &Frames<'_>, frame: usize) -> bool {
        match self.basis {
            Basis::Mark { .. } => self.during(frames, frame) && !frames.novel(self.frame, frame),
            Basis::Click(_) | Basis::SceneChange | Basis::Cue { .. } | Basis::Middle => {
                self.near(frames, frame)
            },
        }
    }

    /// Whether `frame` is within [`FRAME_GAP`] of this keyframe's frame.
    fn near(&self, frames: &Frames<'_>, frame: usize) -> bool {
        frames.at(self.frame).distance(frames.at(frame)) < FRAME_GAP
    }

    /// Whether `frame` came while this keyframe's ink or action was under way, or close to its
    /// frame.
    fn during(&self, frames: &Frames<'_>, frame: usize) -> bool {
        self.near(frames, frame) || self.span.contains(frames.at(frame))
    }
}

/// The `candidates`, taken in rank order, that each add something those taken before them do not
/// show, less any spoken cue whose screen another kind of keyframe already shows.
fn choose(candidates: Vec<Candidate>, frames: &Frames<'_>) -> Vec<Candidate> {
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

/// Whether `candidate` adds something `chosen` does not already show. A mark always does. Nothing
/// else does where a chosen keyframe already covers its frame, so a crowded click a mark covers is
/// reported on that mark's keyframe instead; a scene change or cue must also show a new screen.
fn admits(chosen: &[Candidate], candidate: &Candidate, frames: &Frames<'_>) -> bool {
    let covered = || {
        chosen
            .iter()
            .any(|other| other.covers(frames, candidate.frame))
    };
    let novel = || {
        chosen
            .iter()
            .all(|other| frames.novel(other.frame, candidate.frame))
    };
    match candidate.basis {
        Basis::Mark { .. } => true,
        Basis::Click(_) | Basis::Middle => !covered(),
        Basis::SceneChange | Basis::Cue { .. } => !covered() && novel(),
    }
}

/// Keeps at most `most` of `chosen`, which is in time order, spread across the recording.
///
/// Marks stay, since [`group_marks`] has already fit them, and so, while there is room, does the
/// last keyframe, which shows the recording's last screen. Each other keyframe kept in turn splits
/// the longest stretch still without one, preferring a new screen to a repeat of the screen before
/// it, then the stretch's middle.
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

/// A chosen keyframe as [`thin`] weighs it.
struct Slot {
    at: Moment,
    /// Whether it shows a different screen from the keyframe chosen before it.
    new_screen: bool,
    kept: bool,
}

/// The slot not yet kept that best splits the longest stretch between kept slots, the
/// recording's start, and `end`: a new screen before a repeated one, then the nearest the middle.
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

/// Gives each chosen mark the last click or drag without a keyframe of its own whose result came
/// while its ink was drawn or close to its frame; an action two marks qualify for goes to the
/// nearer.
fn fold_actions_into_marks(chosen: &mut [Candidate], actions: &[Action], frames: &Frames<'_>) {
    for &action in actions {
        let shown = chosen
            .iter()
            .any(|candidate| matches!(candidate.basis, Basis::Click(own) if own == action));
        if shown {
            continue;
        }
        let result = frames.result_of(action);
        let nearest = chosen
            .iter_mut()
            .filter(|candidate| {
                matches!(candidate.basis, Basis::Mark { .. }) && candidate.during(frames, result)
            })
            .min_by_key(|candidate| frames.at(candidate.frame).distance(frames.at(result)));
        if let Some(Basis::Mark { during, .. }) = nearest.map(|mark| &mut mark.basis) {
            *during = Some(action);
        }
    }
}

/// Actions in time order with double clicks merged into their last click.
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

/// Whether `later` is the second click of a double click that `earlier` began.
fn is_double_click(earlier: Action, later: Action) -> bool {
    matches!(
        (earlier, later),
        (Action::Click { at: first, point: here }, Action::Click { at: second, point: there })
            if second.since(first) <= DOUBLE_CLICK && here.distance(there) <= DOUBLE_CLICK_DISTANCE
    )
}

#[cfg(test)]
mod tests;
