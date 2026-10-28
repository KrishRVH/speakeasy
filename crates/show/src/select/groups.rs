//! Gathering marks drawn on one screen into groups that share a keyframe.

use std::{cmp::Ordering, ops::Range, time::Duration};

use super::frames::Frames;
use crate::{Moment, Rect, marks::Mark};

/// Marks share a keyframe while the screen around their ink, grown by [`INK_CONTEXT`] of the
/// frame, changed less than this.
const SAME_SCREEN: f32 = 0.05;
const INK_CONTEXT: f32 = 0.08;
/// A group of marks closes past this span or this much ink, keeping one frame legible.
const GROUP_SPAN: Duration = Duration::from_secs(20);
const GROUP_STROKES: usize = 12;

/// Groups marks drawn on one screen with nothing clicked between them, then merges neighboring
/// groups until at most `most` remain: same-screen pairs carrying the least ink first, else the
/// most alike screens.
pub(super) fn group_marks(
    frames: &Frames<'_>,
    marks: &[Mark],
    actions: &[Moment],
    most: usize,
) -> Vec<Range<usize>> {
    let grouping = Grouping {
        frames,
        marks,
        shown_on: marks
            .iter()
            .map(|mark| frames.nearest(mark.span.end()))
            .collect(),
    };
    grouping.merge(grouping.group(actions), most)
}

/// The marks to group, and the frame each is shown on.
struct Grouping<'a> {
    frames: &'a Frames<'a>,
    marks: &'a [Mark],
    shown_on: Vec<usize>,
}

impl Grouping<'_> {
    /// Groups marks in drawing order. A mark joins the group before it while the group, with it,
    /// stays within [`GROUP_SPAN`] and [`GROUP_STROKES`], no action in `actions` started since the
    /// group's last mark, and the screen around their ink stays the same.
    fn group(&self, actions: &[Moment]) -> Vec<Range<usize>> {
        let acted_between = |after: Moment, before: Moment| {
            actions.partition_point(|at| *at <= after) < actions.partition_point(|at| *at < before)
        };
        let mut groups: Vec<Range<usize>> = Vec::new();
        for (index, mark) in self.marks.iter().enumerate() {
            let joins = groups.last().is_some_and(|group| {
                let (Some(first), Some(previous)) = (
                    self.marks.get(group.start),
                    self.marks.get(group.end.saturating_sub(1)),
                ) else {
                    return false;
                };
                mark.span.end().since(first.span.start()) <= GROUP_SPAN
                    && self.ink(group).saturating_add(mark.strokes.len()) <= GROUP_STROKES
                    && !acted_between(previous.span.end(), mark.span.start())
                    && self.spread(group.clone(), index) < SAME_SCREEN
            });
            match groups.last_mut() {
                Some(group) if joins => group.end = index.saturating_add(1),
                _ => groups.push(index..index.saturating_add(1)),
            }
        }
        groups
    }

    /// Merges neighboring `groups`, cheapest first, until at most `most` remain.
    fn merge(&self, mut groups: Vec<Range<usize>>, most: usize) -> Vec<Range<usize>> {
        if groups.len() <= most {
            return groups;
        }
        let mut costs: Vec<Option<Merge>> = (0..groups.len())
            .map(|k| self.merge_cost(&groups, k))
            .collect();
        while groups.len() > most {
            let cheapest = costs
                .iter()
                .enumerate()
                .filter_map(|(k, cost)| cost.map(|cost| (k, cost)))
                .min_by(|(_, one), (_, other)| one.total_cmp(other))
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
                let refreshed = self.merge_cost(&groups, neighbor);
                if let Some(slot) = costs.get_mut(neighbor) {
                    *slot = refreshed;
                }
            }
        }
        groups
    }

    /// What merging group `k` of `groups` into the one before it costs; `None` for the first.
    fn merge_cost(&self, groups: &[Range<usize>], k: usize) -> Option<Merge> {
        let merged = groups.get(k.checked_sub(1)?)?.start..groups.get(k)?.end;
        let spread = self.spread(merged.clone(), merged.end.saturating_sub(1));
        Some(if spread < SAME_SCREEN {
            Merge::SameScreen {
                ink: self.ink(&merged),
            }
        } else {
            Merge::AcrossScreens { spread }
        })
    }

    /// The strokes `group`'s marks carry.
    fn ink(&self, group: &Range<usize>) -> usize {
        self.marks
            .get(group.clone())
            .unwrap_or_default()
            .iter()
            .map(|mark| mark.strokes.len())
            .sum()
    }

    /// How far the screen around the ink, as each mark of `group` was drawn, is from the screen
    /// mark `last` is shown on.
    fn spread(&self, group: Range<usize>, last: usize) -> f32 {
        let Some(around) = self
            .marks
            .get(group.start..group.end.max(last.saturating_add(1)))
            .and_then(|members| members.iter().map(|mark| mark.bounds).reduce(Rect::union))
        else {
            return 1.0;
        };
        let Some(&shown) = self.shown_on.get(last) else {
            return 1.0;
        };
        let around = around.expanded_within(INK_CONTEXT, Rect::UNIT);
        let shown = self.frames.signature(shown);
        group
            .filter_map(|member| self.shown_on.get(member))
            .map(|&frame| self.frames.signature(frame).region_change(shown, around))
            .fold(0.0, f32::max)
    }
}

/// What merging a group into the one before it costs: any merge onto one screen is cheaper than
/// any across screens.
#[derive(Clone, Copy)]
enum Merge {
    /// Onto one screen, by the strokes the merged group carries.
    SameScreen { ink: usize },
    /// Across screens, by how far apart they are.
    AcrossScreens { spread: f32 },
}

impl Merge {
    fn total_cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::SameScreen { ink: one }, Self::SameScreen { ink: other }) => one.cmp(other),
            (Self::AcrossScreens { spread: one }, Self::AcrossScreens { spread: other }) => {
                one.total_cmp(other)
            },
            (Self::SameScreen { .. }, Self::AcrossScreens { .. }) => Ordering::Less,
            (Self::AcrossScreens { .. }, Self::SameScreen { .. }) => Ordering::Greater,
        }
    }
}
