//! Choosing the frames that tell a Show's story.

use std::{fmt, ops::Range, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{
    Moment, Point, Rect, Span, align,
    marks::{self, At, Mark},
    recording::{Action, Recording, Word},
    signature::{Sample, Signature},
};

/// Frames are sampled this often: fast enough to catch what a click changed, slow enough that a
/// still screen costs nothing and a busy one stays cheap.
///
/// How long a click's result settles and how far apart keyframes keep are tuned against it.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// A click's keyframe shows the screen this long after release, once the app has responded.
const SETTLE: Duration = Duration::from_millis(250);
/// Keyframes other than marks keep at least this far apart.
const FRAME_GAP: Duration = Duration::from_millis(750);
/// Clicks, scene changes, and cues are added only while fewer keyframes than this are chosen;
/// beyond it, only marks and long stretches of narration earn one.
const ORDINARY: usize = 6;
/// A transition at least this strong is a scene change.
const SCENE: f32 = 0.08;
/// Scene changes and spoken cues must differ at least this much from every chosen keyframe.
const NOVELTY: f32 = 0.03;
/// Marks share a keyframe while the screen around their ink, grown by [`INK_CONTEXT`] of the
/// frame, changed less than this.
const SAME_SCREEN: f32 = 0.05;
const INK_CONTEXT: f32 = 0.08;
/// A group of marks closes past this span or this much ink, keeping one frame legible.
const GROUP_SPAN: Duration = Duration::from_secs(20);
const GROUP_STROKES: usize = 12;
/// A keyframe owns too much narration past this many words or this long.
const COVERAGE_WORDS: f32 = 100.0;
const COVERAGE_TIME: Duration = Duration::from_secs(35);
/// Clicks this close in time and place are one click.
const DOUBLE_CLICK: Duration = Duration::from_millis(650);
const DOUBLE_CLICK_DISTANCE: f32 = 0.03;
/// A spoken cue points at a transition starting from just before the word to a little after.
const CUE_LEAD: Duration = Duration::from_millis(300);
const CUE_REACH: Duration = Duration::from_millis(1_200);
/// Without a transition, a cue shows the screen as the word was said.
const CUE_LOOK: Duration = Duration::from_millis(150);
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
    /// The click or drag whose result it shows.
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
    /// Ink grouped into gestures, in drawing order.
    #[must_use]
    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

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
/// A cue never repeats a screen already chosen, so talking over a still screen yields one frame.
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
    let drawing: Vec<Span> = candidates.iter().map(|candidate| candidate.span).collect();
    candidates.extend(
        actions
            .iter()
            .map(|&action| frames.action_candidate(action)),
    );
    candidates.extend(
        transitions
            .iter()
            .filter(|transition| {
                let settled = frames.at(transition.frame);
                !drawing.iter().any(|span| span.contains(settled))
            })
            .map(Transition::candidate),
    );
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
            event: event(&candidate.basis, &marks),
            said,
            marks: candidate.basis.marks(),
            action: candidate.basis.action(),
        })
        .collect();
    (!keyframes.is_empty()).then_some(Selection { marks, keyframes })
}

/// The samples in time order. Every frame index this view hands out is in bounds.
struct Frames<'a> {
    samples: Vec<&'a Sample>,
    first: &'a Sample,
}

impl<'a> Frames<'a> {
    fn new(samples: &'a [Sample]) -> Option<Self> {
        let mut samples: Vec<&Sample> = samples.iter().collect();
        samples.sort_by_key(|sample| sample.at);
        Some(Self {
            first: samples.first().copied()?,
            samples,
        })
    }

    fn sample(&self, frame: usize) -> &'a Sample {
        self.samples.get(frame).copied().unwrap_or(self.first)
    }

    fn at(&self, frame: usize) -> Moment {
        self.sample(frame).at
    }

    fn signature(&self, frame: usize) -> &'a Signature {
        &self.sample(frame).signature
    }

    fn last(&self) -> usize {
        self.samples.len().saturating_sub(1)
    }

    /// The frame on screen closest to `moment`.
    fn nearest(&self, moment: Moment) -> usize {
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
    fn transitions(&self) -> Vec<Transition> {
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

    fn mark_candidate(&self, marks: &[Mark], group: Range<usize>) -> Option<Candidate> {
        let members = marks.get(group.clone())?;
        let span = members.iter().map(|mark| mark.span).reduce(Span::union)?;
        Some(Candidate {
            frame: self.nearest(span.end()),
            span,
            score: 100.0,
            basis: Basis::Mark(group),
        })
    }

    fn action_candidate(&self, action: Action) -> Candidate {
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

    fn cue_candidate(&self, word: &Word, transitions: &[Transition]) -> Option<Candidate> {
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

    fn middle_candidate(&self) -> Candidate {
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
struct Transition {
    frame: usize,
    span: Span,
    strength: f32,
}

impl Transition {
    fn score(self) -> f32 {
        40.0f32.mul_add(self.strength, 40.0)
    }

    fn candidate(&self) -> Candidate {
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

struct Candidate {
    frame: usize,
    span: Span,
    score: f32,
    basis: Basis,
}

/// What a candidate frame shows, which decides how it ranks and what its keyframe says.
#[derive(Clone, Debug)]
pub(crate) enum Basis {
    /// The selection's marks in this range, drawn on one screen.
    Mark(Range<usize>),
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
            Self::Mark(_) => Reason::Mark,
            Self::Click(_) => Reason::Click,
            Self::SceneChange => Reason::SceneChange,
            Self::Cue { .. } => Reason::Cue,
            Self::Middle => Reason::Middle,
        }
    }

    fn marks(&self) -> Range<usize> {
        match self {
            Self::Mark(group) => group.clone(),
            Self::Click(_) | Self::SceneChange | Self::Cue { .. } | Self::Middle => 0..0,
        }
    }

    const fn action(&self) -> Option<Action> {
        match *self {
            Self::Click(action) => Some(action),
            Self::Mark(_) | Self::SceneChange | Self::Cue { .. } | Self::Middle => None,
        }
    }
}

/// What a keyframe chosen for `basis` shows, for a reader; `marks` are the selection's.
fn event(basis: &Basis, marks: &[Mark]) -> String {
    match basis {
        Basis::Mark(group) => marks
            .get(group.clone())
            .unwrap_or_default()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
        Basis::Click(action) => Described(*action).to_string(),
        Basis::SceneChange => "scene changed".to_owned(),
        Basis::Cue {
            word,
            snapped: true,
        } => format!("said “{word}” as the screen changed"),
        Basis::Cue {
            word,
            snapped: false,
        } => format!("said “{word}”"),
        Basis::Middle => "the screen midway through".to_owned(),
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
            ordinary: !matches!(self.basis, Basis::Mark(_)),
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
        if matches!(candidate.basis, Basis::Mark(_)) {
            return true;
        }
        let at = self.frames.at(candidate.frame);
        let crowded = self.chosen.iter().any(|other| {
            self.frames.at(other.frame).distance(at) < FRAME_GAP
                || (matches!(&other.basis, Basis::Mark(group) if group.len() > 1)
                    && other.span.contains(at))
        });
        if crowded {
            return false;
        }
        !matches!(candidate.basis, Basis::Cue { .. } | Basis::SceneChange)
            || self
                .chosen
                .iter()
                .all(|other| self.novel(other.frame, candidate.frame))
    }

    fn novel(&self, one: usize, other: usize) -> bool {
        self.frames
            .signature(one)
            .transition(self.frames.signature(other))
            >= NOVELTY
    }

    fn full(&self, limit: usize) -> bool {
        self.chosen.len() >= limit.min(self.budget)
    }

    fn choose_marks(&mut self) {
        for slot in &mut self.pending {
            if self.chosen.len() >= self.budget
                || slot
                    .as_ref()
                    .is_none_or(|c| !matches!(c.basis, Basis::Mark(_)))
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
        if self.chosen.len() > 1 && !shown.is_empty() {
            let frames = self.frames;
            self.chosen.retain(|candidate| {
                !matches!(candidate.basis, Basis::Cue { .. })
                    || shown.iter().all(|&other| {
                        frames
                            .signature(other)
                            .transition(frames.signature(candidate.frame))
                            >= NOVELTY
                    })
            });
        }
    }

    /// Splits stretches of narration a keyframe owns that run past [`COVERAGE_WORDS`] or
    /// [`COVERAGE_TIME`], with a frame from inside them where the screen changed.
    ///
    /// The best-ranked candidate that leaves no part above three quarters of the stretch wins,
    /// else the most even split. The first keyframe also owns the lead-in before it, but a frame
    /// added there must split the lead-in evenly too, so it lands mid-gap rather than beside the
    /// first keyframe.
    fn cover_long_stretches(&mut self, words: &[Word]) {
        let coverage = Coverage::new(words, self.end());
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

    fn end(&self) -> Moment {
        self.frames.at(self.frames.last())
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
            .windows(2)
            .filter_map(|pair| Some(Span::new(*pair.first()?, *pair.last()?)))
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

/// Groups marks drawn on one screen with nothing clicked between them, then merges neighboring
/// groups until they fit `budget`: same-screen pairs carrying the least ink first, else the most
/// alike screens.
fn group_marks(
    frames: &Frames<'_>,
    marks: &[Mark],
    actions: &[Moment],
    budget: usize,
) -> Vec<Range<usize>> {
    let frame_of: Vec<usize> = marks
        .iter()
        .map(|mark| frames.nearest(mark.span.end()))
        .collect();
    let ink = |group: &Range<usize>| -> usize {
        marks
            .get(group.clone())
            .unwrap_or_default()
            .iter()
            .map(Mark::stroke_count)
            .sum()
    };
    // How far the screen around the ink, as each mark of `group` was drawn, is from the screen
    // where `last` will be shown.
    let spread = |group: Range<usize>, last: usize| -> f32 {
        let Some(around) = marks
            .get(group.start..group.end.max(last.saturating_add(1)))
            .and_then(|members| members.iter().map(|mark| mark.bounds).reduce(Rect::union))
        else {
            return 1.0;
        };
        let around = around.expanded_within(INK_CONTEXT, Rect::UNIT);
        let shown = frame_of.get(last).map(|&frame| frames.signature(frame));
        group
            .filter_map(|member| frame_of.get(member))
            .map(|&frame| {
                shown.map_or(1.0, |shown| {
                    frames.signature(frame).region_change(shown, around)
                })
            })
            .fold(0.0, f32::max)
    };
    let acted_between = |after: Moment, before: Moment| {
        actions.partition_point(|at| *at <= after) < actions.partition_point(|at| *at < before)
    };

    let mut groups: Vec<Range<usize>> = Vec::new();
    for (index, mark) in marks.iter().enumerate() {
        let joins = groups.last().is_some_and(|group| {
            let (Some(first), Some(previous)) = (
                marks.get(group.start),
                marks.get(group.end.saturating_sub(1)),
            ) else {
                return false;
            };
            mark.span.end().since(first.span.start()) <= GROUP_SPAN
                && ink(group).saturating_add(mark.stroke_count()) <= GROUP_STROKES
                && !acted_between(previous.span.end(), mark.span.start())
                && spread(group.clone(), index) < SAME_SCREEN
        });
        match groups.last_mut() {
            Some(group) if joins => group.end = index.saturating_add(1),
            _ => groups.push(index..index.saturating_add(1)),
        }
    }

    // Merging group `k` into `k - 1` costs the ink they'd carry together on one screen, or how
    // far apart their screens are; a merge across screens always costs more.
    let cost = |groups: &[Range<usize>], k: usize| -> Option<(bool, f32)> {
        let merged = groups.get(k.checked_sub(1)?)?.start..groups.get(k)?.end;
        let spread = spread(merged.clone(), merged.end.saturating_sub(1));
        Some(if spread < SAME_SCREEN {
            (false, ink(&merged) as f32)
        } else {
            (true, spread)
        })
    };
    let mut costs: Vec<Option<(bool, f32)>> = (0..groups.len()).map(|k| cost(&groups, k)).collect();
    while groups.len() > budget {
        let cheapest = costs
            .iter()
            .enumerate()
            .filter_map(|(k, cost)| cost.map(|cost| (k, cost)))
            .min_by(|(_, one), (_, other)| one.0.cmp(&other.0).then(one.1.total_cmp(&other.1)))
            .map(|(k, _)| k);
        let Some(k) = cheapest else {
            break;
        };
        let later = groups.remove(k);
        costs.remove(k);
        if let Some(earlier) = k
            .checked_sub(1)
            .and_then(|previous| groups.get_mut(previous))
        {
            earlier.end = later.end;
        }
        for neighbor in [k.saturating_sub(1), k] {
            let refreshed = cost(&groups, neighbor);
            if let Some(slot) = costs.get_mut(neighbor) {
                *slot = refreshed;
            }
        }
    }
    groups
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

/// What an action did, in a word or two, such as "clicked" or "dragged right".
pub(crate) struct Verb(pub(crate) Action);

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Action::Click { .. } => f.write_str("clicked"),
            Action::Drag { from, to, .. } => write!(f, "dragged {}", Direction(to - from)),
        }
    }
}

/// An action described for a reader, with where it happened.
struct Described(Action);

impl fmt::Display for Described {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verb = Verb(self.0);
        match self.0 {
            Action::Click { point, .. } => write!(f, "{verb} ({})", At(point)),
            Action::Drag { from, to, .. } => write!(f, "{verb} ({} → {})", At(from), At(to)),
        }
    }
}

/// The main direction of a movement, or "a little" when it barely moved.
struct Direction(Point);

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MOVED: f32 = 0.04;
        let Point { x, y } = self.0;
        let horizontal = (x.abs() > MOVED).then_some(if x > 0.0 { "right" } else { "left" });
        let vertical = (y.abs() > MOVED).then_some(if y > 0.0 { "down" } else { "up" });
        match (vertical, horizontal) {
            (_, Some(horizontal)) if x.abs() > 2.0 * y.abs() => f.write_str(horizontal),
            (Some(vertical), _) if y.abs() > 2.0 * x.abs() => f.write_str(vertical),
            (Some(vertical), Some(horizontal)) => write!(f, "{vertical} and {horizontal}"),
            (Some(only), None) | (None, Some(only)) => f.write_str(only),
            (None, None) => f.write_str("a little"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InkPoint, Narration, Stroke, recording::Size, signature::tests::signature};

    fn sample(at: u64, level: u8) -> Sample {
        Sample {
            at: Moment::from_millis(at),
            signature: signature(level),
        }
    }

    /// Samples every `step` milliseconds through `end`, gray at the level `level_at` gives.
    fn samples(step: usize, end: u64, level_at: impl Fn(u64) -> u8) -> Vec<Sample> {
        (0..=end)
            .step_by(step)
            .map(|at| sample(at, level_at(at)))
            .collect()
    }

    /// `count` moments `step` milliseconds apart from `first`.
    fn every(first: u64, step: usize, count: usize) -> impl Iterator<Item = u64> {
        (first..).step_by(step).take(count)
    }

    /// A dab of ink lifted at `at`.
    fn dab(at: u64) -> Stroke {
        Stroke::new(
            [at.saturating_sub(100), at]
                .map(|at| InkPoint {
                    point: Point::new(0.5, 0.5),
                    at: Moment::from_millis(at),
                })
                .to_vec(),
        )
        .unwrap()
    }

    fn click(at: u64) -> Action {
        Action::Click {
            at: Moment::from_millis(at),
            point: Point::new(0.5, 0.5),
        }
    }

    fn words(list: &[(u64, &str)]) -> Narration {
        Narration::Words(
            list.iter()
                .map(|&(at, text)| Word {
                    span: Span::new(
                        Moment::from_millis(at),
                        Moment::from_millis(at.saturating_add(200)),
                    ),
                    text: text.to_owned(),
                })
                .collect(),
        )
    }

    fn recording(strokes: Vec<Stroke>, actions: Vec<Action>, narration: Narration) -> Recording {
        Recording {
            duration: Moment::from_millis(60_000),
            surface: Size {
                width: 1000.0,
                height: 1000.0,
            },
            strokes,
            actions,
            narration,
        }
    }

    fn times(selection: &Selection) -> Vec<u64> {
        selection
            .keyframes
            .iter()
            .map(|keyframe| keyframe.at.as_millis())
            .collect()
    }

    fn said(selection: &Selection) -> Vec<Option<&str>> {
        selection
            .keyframes
            .iter()
            .map(|keyframe| keyframe.said.as_deref())
            .collect()
    }

    #[test]
    fn marks_on_one_screen_share_a_keyframe() {
        let frames = [
            sample(0, 0),
            sample(1_000, 255),
            sample(2_000, 255),
            sample(4_000, 255),
        ];
        let shown = select_within(
            &recording(
                vec![dab(2_000), dab(4_000)],
                vec![click(750)],
                Narration::default(),
            ),
            &frames,
            2,
        )
        .unwrap();
        assert_eq!(times(&shown), [1_000, 4_000]);
        assert_eq!(shown.keyframes()[0].reason, Reason::Click);
        assert_eq!(shown.keyframes()[1].reason, Reason::Mark);
        assert_eq!(shown.keyframes()[1].marks, 0..2);
        assert_eq!(shown.keyframes()[1].file(), "frames/frame-02.png");
    }

    #[test]
    fn every_mark_gets_a_keyframe_within_the_budget() {
        let levels = [0, 40, 80, 120, 160, 164, 210, 250];
        let frames: Vec<Sample> = every(1_000, 2_000, 8)
            .zip(levels)
            .map(|(at, level)| sample(at, level))
            .collect();
        let strokes = every(1_000, 2_000, 8).map(dab).collect();
        let shown = select_within(
            &recording(strokes, Vec::new(), Narration::default()),
            &frames,
            7,
        )
        .unwrap();
        assert_eq!(shown.keyframes().len(), 7);
        let covered: Vec<Range<usize>> =
            shown.keyframes().iter().map(|k| k.marks.clone()).collect();
        assert_eq!(covered, [0..1, 1..2, 2..3, 3..4, 4..6, 6..7, 7..8]);
        assert_eq!(shown.keyframes()[4].at.as_millis(), 11_000);

        // On one unchanged screen, marks group until they span more than twenty seconds.
        let still = samples(1_000, 50_000, |_| 128);
        let strokes = [2_000, 9_000, 20_000, 33_000, 40_000].map(dab).to_vec();
        let shown = select_within(
            &recording(strokes, Vec::new(), Narration::default()),
            &still,
            12,
        )
        .unwrap();
        assert_eq!(times(&shown), [20_000, 40_000]);
    }

    #[test]
    fn a_scene_change_shows_the_settled_view() {
        let frames = [
            sample(0, 0),
            sample(1_000, 80),
            sample(1_250, 160),
            sample(1_500, 255),
            sample(1_750, 255),
        ];
        let shown = select_within(&Recording::default(), &frames, 12).unwrap();
        assert_eq!(times(&shown), [1_500]);
        assert_eq!(shown.keyframes()[0].reason, Reason::SceneChange);
    }

    #[test]
    fn double_clicks_merge_and_keyframes_keep_apart() {
        let frames = samples(250, 3_000, |_| 128);
        let actions = vec![click(1_000), click(1_100), click(2_300)];
        let shown = select_within(
            &recording(Vec::new(), actions, Narration::default()),
            &frames,
            12,
        )
        .unwrap();
        assert_eq!(times(&shown), [1_250, 2_500]);
        assert_eq!(shown.keyframes()[0].event, "clicked (x 0.50, y 0.50)");
    }

    #[test]
    fn narration_divides_among_marks() {
        let frames = [sample(1_000, 0), sample(4_000, 255)];
        let narration = words(&[
            (0, "Make"),
            (1_000, "this"),
            (2_000, "smaller,"),
            (3_000, "and"),
            (4_000, "move"),
            (7_000, "that."),
        ]);
        let shown = select_within(
            &recording(vec![dab(1_000), dab(4_000)], Vec::new(), narration),
            &frames,
            12,
        )
        .unwrap();
        assert_eq!(
            said(&shown),
            [Some("Make this smaller,"), Some("and move that.")]
        );
    }

    #[test]
    fn spoken_cues_never_repeat_an_unchanged_screen() {
        let still = samples(250, 5_000, |_| 90);
        let narration = words(&[
            (500, "Look"),
            (700, "at"),
            (900, "this."),
            (3_000, "Now"),
            (3_200, "it's"),
            (3_400, "stuck."),
        ]);
        let shown =
            select_within(&recording(Vec::new(), Vec::new(), narration), &still, 12).unwrap();
        assert_eq!(shown.keyframes().len(), 1);
        assert_eq!(shown.keyframes()[0].reason, Reason::Cue);
        assert_eq!(said(&shown), [Some("Look at this. Now it's stuck.")]);

        // A cue over a screen a scene change already settled on adds nothing.
        let changed = samples(250, 5_000, |at| if at < 1_000 { 40 } else { 90 });
        let narration = words(&[(3_500, "Look"), (3_700, "here.")]);
        let shown =
            select_within(&recording(Vec::new(), Vec::new(), narration), &changed, 12).unwrap();
        assert_eq!(shown.keyframes().len(), 1);
        assert_eq!(shown.keyframes()[0].reason, Reason::SceneChange);
    }

    #[test]
    fn a_spoken_cue_finds_the_change_it_points_at() {
        let frames = samples(250, 6_000, |at| if at < 3_250 { 40 } else { 220 });
        let narration = words(&[
            (200, "Here's"),
            (400, "the"),
            (600, "list."),
            (2_400, "Watch"),
            (2_600, "it"),
            (2_800, "now."),
        ]);
        let shown = select_within(
            &recording(Vec::new(), vec![click(500)], narration),
            &frames,
            12,
        )
        .unwrap();
        assert_eq!(times(&shown), [750, 3_250]);
        assert_eq!(shown.keyframes()[1].reason, Reason::Cue);
        assert_eq!(
            shown.keyframes()[1].event,
            "said “watch” as the screen changed"
        );
        assert_eq!(
            said(&shown),
            [Some("Here's the list."), Some("Watch it now.")]
        );
    }

    #[test]
    fn groups_close_at_a_click_or_too_much_ink() {
        let still = samples(250, 10_000, |_| 128);
        let shown = select_within(
            &recording(
                vec![dab(2_000), dab(4_000)],
                vec![click(3_000)],
                Narration::default(),
            ),
            &still,
            12,
        )
        .unwrap();
        let marks: Vec<Range<usize>> = shown.keyframes().iter().map(|k| k.marks.clone()).collect();
        assert!(
            marks.contains(&(0..1)) && marks.contains(&(1..2)),
            "{marks:?}"
        );

        // Five strokes a mark: two marks fit a group, a third would carry too much ink.
        let heavy = |at: u64| every(at.saturating_sub(400), 100, 5).map(dab);
        let strokes = heavy(2_000)
            .chain(heavy(4_000))
            .chain(heavy(6_000))
            .collect();
        let shown = select_within(
            &recording(strokes, Vec::new(), Narration::default()),
            &still,
            12,
        )
        .unwrap();
        let marks: Vec<Range<usize>> = shown.keyframes().iter().map(|k| k.marks.clone()).collect();
        assert_eq!(marks, [0..2, 2..3]);
    }

    #[test]
    fn same_screen_merges_stay_balanced() {
        let still = samples(250, 15_000, |_| 128);
        let strokes = every(1_000, 2_000, 6).map(dab).collect();
        let clicks = every(2_000, 2_000, 5).map(click).collect();
        let shown =
            select_within(&recording(strokes, clicks, Narration::default()), &still, 3).unwrap();
        let marks: Vec<Range<usize>> = shown.keyframes().iter().map(|k| k.marks.clone()).collect();
        assert_eq!(marks, [0..2, 2..4, 4..6]);
    }

    #[test]
    fn nothing_cuts_into_a_drawing_session() {
        let still = samples(250, 10_000, |_| 128);
        let narration = words(&[(4_000, "look")]);
        let shown = select_within(
            &recording(vec![dab(2_000), dab(6_000)], Vec::new(), narration),
            &still,
            12,
        )
        .unwrap();
        assert_eq!(shown.keyframes().len(), 1);
        assert_eq!(shown.keyframes()[0].reason, Reason::Mark);
    }

    #[test]
    fn long_talk_over_a_changing_screen_gets_more_than_one_frame() {
        // Two minutes of steady talk. Small changes at 0:30 and 1:00, then bigger ones near the
        // end that outrank them: ranked alone, the first keyframe would own a minute and a half.
        let level = |at: u64| match at {
            0..30_000 => 20,
            30_000..60_000 => 50,
            60_000..90_000 => 80,
            90_000..100_000 => 180,
            100_000..110_000 => 250,
            110_000..115_000 => 140,
            _ => 220,
        };
        let frames = samples(500, 120_000, level);
        let talk: Vec<(u64, &str)> = every(0, 500, 240).map(|at| (at, "word")).collect();
        let shown =
            select_within(&recording(Vec::new(), Vec::new(), words(&talk)), &frames, 4).unwrap();
        let at = times(&shown);
        assert_eq!(at.len(), 4, "{at:?}");
        assert!(at[0] <= 30_000 && at[1] <= 60_000, "{at:?}");
        let most = shown
            .keyframes
            .iter()
            .map(|keyframe| {
                keyframe
                    .said
                    .as_deref()
                    .unwrap_or("")
                    .split_whitespace()
                    .count()
            })
            .max();
        assert!(most <= Some(120), "{most:?}");

        // The same talk over an unchanged screen stays one frame.
        let still = samples(500, 120_000, |_| 90);
        let shown =
            select_within(&recording(Vec::new(), Vec::new(), words(&talk)), &still, 12).unwrap();
        assert_eq!(shown.keyframes().len(), 1);
    }

    #[test]
    fn a_long_lead_in_is_split_near_its_middle() {
        // One strong late change ranks first; of the small earlier ones, coverage takes the one
        // nearest the middle of the lead-in, not the one beside the late frame.
        let level = |at: u64| match at {
            0..30_000 => 20,
            30_000..70_000 => 50,
            70_000..130_000 => 80,
            130_000..138_000 => 110,
            _ => 210,
        };
        let frames = samples(500, 150_000, level);
        let shown = select_within(&Recording::default(), &frames, 2).unwrap();
        assert_eq!(times(&shown), [70_000, 138_000]);
    }

    #[test]
    fn the_budget_grows_with_duration() {
        let budgets =
            [8, 39, 40, 67, 140, 268, u64::MAX].map(|seconds| budget(Duration::from_secs(seconds)));
        assert_eq!(budgets, [6, 6, 7, 8, 12, 12, 12]);
    }

    #[test]
    fn each_event_says_why_its_frame_was_chosen() {
        let event = |shown: Option<Selection>| shown.unwrap().keyframes()[0].event.clone();
        let still = samples(250, 10_000, |_| 128);
        let drawn = recording(
            vec![dab(2_000), dab(4_000)],
            Vec::new(),
            Narration::default(),
        );
        assert_eq!(
            event(select_within(&drawn, &still, 12)),
            "tapped x 0.50, y 0.50; tapped x 0.50, y 0.50"
        );
        let looked = recording(Vec::new(), Vec::new(), words(&[(3_000, "Look")]));
        assert_eq!(event(select_within(&looked, &still, 12)), "said “look”");
        let changed = [
            sample(0, 0),
            sample(1_000, 80),
            sample(1_250, 160),
            sample(1_500, 255),
        ];
        assert_eq!(
            event(select_within(&Recording::default(), &changed, 12)),
            "scene changed"
        );
        let quiet = [sample(0, 0), sample(1_000, 0), sample(2_000, 0)];
        assert_eq!(
            event(select_within(&Recording::default(), &quiet, 12)),
            "the screen midway through"
        );
    }

    #[test]
    fn a_quiet_show_falls_back_to_its_middle() {
        let frames = [sample(0, 0), sample(1_000, 0), sample(2_000, 0)];
        let shown = select_within(&Recording::default(), &frames, 12).unwrap();
        assert_eq!(times(&shown), [1_000]);
        assert_eq!(shown.keyframes()[0].reason, Reason::Middle);
        assert_eq!(select_within(&Recording::default(), &frames, 0), None);
        assert_eq!(select_within(&Recording::default(), &[], 12), None);
    }

    proptest::proptest! {
        #[test]
        fn selection_keeps_its_invariants(
            gaps in proptest::collection::vec((50_u64..2_000, 0_u8..=255), 1..40),
            dabs in proptest::collection::vec(100_u64..60_000, 0..10),
            clicks in proptest::collection::vec(0_u64..60_000, 0..6),
            spoken in proptest::collection::vec((0_u64..60_000, 0_usize..6), 0..30),
            budget in 0_usize..16,
        ) {
            let frames: Vec<Sample> = gaps
                .iter()
                .scan(0_u64, |at, &(gap, level)| {
                    *at = at.saturating_add(gap);
                    Some(sample(*at, level))
                })
                .collect();
            let mut dabs = dabs;
            dabs.sort_unstable();
            let mut spoken = spoken;
            spoken.sort_unstable();
            let vocabulary = ["look", "now", "this", "works", "fine.", "here,"];
            let narration: Vec<(u64, &str)> = spoken
                .iter()
                .map(|&(at, word)| (at, vocabulary[word]))
                .collect();
            let recording = recording(
                dabs.into_iter().map(dab).collect(),
                clicks.into_iter().map(click).collect(),
                words(&narration),
            );
            let shown = select_within(&recording, &frames, budget);
            proptest::prop_assert_eq!(shown.is_some(), budget > 0);
            let Some(shown) = shown else {
                return Ok(());
            };
            let keyframes = shown.keyframes();
            proptest::prop_assert!(!keyframes.is_empty());
            proptest::prop_assert!(keyframes.len() <= budget.min(MOST_KEYFRAMES));
            proptest::prop_assert!(keyframes.iter().map(|k| k.index).eq(1..=keyframes.len()));
            proptest::prop_assert!(keyframes.windows(2).all(|pair| pair[0].at <= pair[1].at));
            let covered: Vec<usize> = keyframes.iter().flat_map(|k| k.marks.clone()).collect();
            proptest::prop_assert_eq!(covered, (0..shown.marks().len()).collect::<Vec<_>>());
            let mut said: Vec<&str> = keyframes
                .iter()
                .filter_map(|k| k.said.as_deref())
                .flat_map(str::split_whitespace)
                .collect();
            let mut expected: Vec<&str> = narration.iter().map(|(_, word)| *word).collect();
            said.sort_unstable();
            expected.sort_unstable();
            proptest::prop_assert_eq!(said, expected);
        }
    }

    #[test]
    fn drags_describe_their_direction() {
        let drag = |to: [f32; 2]| {
            Described(Action::Drag {
                span: Span::at(Moment::START),
                from: Point::new(0.5, 0.5),
                to: to.into(),
            })
            .to_string()
        };
        assert_eq!(
            drag([0.5, 0.9]),
            "dragged down (x 0.50, y 0.50 → x 0.50, y 0.90)"
        );
        assert_eq!(
            drag([0.2, 0.2]),
            "dragged up and left (x 0.50, y 0.50 → x 0.20, y 0.20)"
        );
        assert_eq!(
            drag([0.51, 0.5]),
            "dragged a little (x 0.50, y 0.50 → x 0.51, y 0.50)"
        );
    }
}
