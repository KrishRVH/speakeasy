use std::{cmp::Ordering, ops::Range, time::Duration};

use super::frames::Frames;
use crate::{Moment, Rect, Span, marks::Mark};

const SAME_SCREEN: f32 = 0.05;
const INK_CONTEXT: f32 = 0.08;

/// A mark group closes past this span or `GROUP_STROKES`, keeping one frame legible.
const GROUP_SPAN: Duration = Duration::from_secs(20);
const GROUP_STROKES: usize = 12;

pub(super) fn group_marks(
    frames: &Frames<'_>,
    marks: &[Mark],
    action_starts: &[Moment],
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
    grouping.merge(grouping.group(action_starts), most)
}

struct Grouping<'a> {
    frames: &'a Frames<'a>,
    marks: &'a [Mark],
    shown_on: Vec<usize>,
}

impl Grouping<'_> {
    fn group(&self, action_starts: &[Moment]) -> Vec<Range<usize>> {
        let acted_between = |after: Moment, before: Moment| {
            action_starts.partition_point(|at| *at <= after)
                < action_starts.partition_point(|at| *at < before)
        };
        let mut groups: Vec<Range<usize>> = Vec::new();
        for (index, mark) in self.marks.iter().enumerate() {
            let joins =
                groups.last().is_some_and(|group| {
                    let Some(span) = self.marks.get(group.clone()).and_then(|members| {
                        members.iter().map(|mark| mark.span).reduce(Span::union)
                    }) else {
                        return false;
                    };
                    let joined = group.start..index.saturating_add(1);
                    span.end().max(mark.span.end()).since(span.start()) <= GROUP_SPAN
                        && self.ink(&joined) <= GROUP_STROKES
                        && !acted_between(span.end(), mark.span.start())
                        && self.spread(joined) < SAME_SCREEN
                });
            match groups.last_mut() {
                Some(group) if joins => group.end = index.saturating_add(1),
                _ => groups.push(index..index.saturating_add(1)),
            }
        }
        groups
    }

    fn merge(&self, mut groups: Vec<Range<usize>>, most: usize) -> Vec<Range<usize>> {
        if groups.len() <= most {
            return groups;
        }
        let mut costs: Vec<Option<Merge>> = (0..groups.len())
            .map(|seam| self.merge_cost(&groups, seam))
            .collect();
        while groups.len() > most {
            let cheapest = costs
                .iter()
                .enumerate()
                .filter_map(|(seam, cost)| cost.map(|cost| (seam, cost)))
                .min_by(|(_, one), (_, other)| one.total_cmp(other))
                .map(|(seam, _)| seam);
            let Some(seam) = cheapest else {
                break;
            };
            let later = groups.remove(seam);
            costs.remove(seam);
            if let Some(earlier) = seam
                .checked_sub(1)
                .and_then(|previous| groups.get_mut(previous))
            {
                earlier.end = later.end;
            }
            for neighbor in [seam.saturating_sub(1), seam] {
                if let Some(slot) = costs.get_mut(neighbor) {
                    *slot = self.merge_cost(&groups, neighbor);
                }
            }
        }
        groups
    }

    fn merge_cost(&self, groups: &[Range<usize>], seam: usize) -> Option<Merge> {
        let merged = groups.get(seam.checked_sub(1)?)?.start..groups.get(seam)?.end;
        let spread = self.spread(merged.clone());
        Some(if spread < SAME_SCREEN {
            Merge::SameScreen {
                ink: self.ink(&merged),
            }
        } else {
            Merge::AcrossScreens { spread }
        })
    }

    fn ink(&self, group: &Range<usize>) -> usize {
        self.marks
            .get(group.clone())
            .unwrap_or_default()
            .iter()
            .map(|mark| mark.strokes.len())
            .sum()
    }

    fn spread(&self, group: Range<usize>) -> f32 {
        let (Some(around), Some(&shown)) = (
            self.marks
                .get(group.clone())
                .and_then(|members| members.iter().map(|mark| mark.bounds).reduce(Rect::union)),
            self.shown_on
                .get(group.clone())
                .and_then(|frames| frames.iter().max()),
        ) else {
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

#[derive(Clone, Copy)]
enum Merge {
    SameScreen { ink: usize },
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
