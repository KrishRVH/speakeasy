//! Gathering marks drawn on one screen into groups that share a keyframe.

use std::{ops::Range, time::Duration};

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
/// groups until they fit `budget`: same-screen pairs carrying the least ink first, else the most
/// alike screens.
pub(super) fn group_marks(
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
