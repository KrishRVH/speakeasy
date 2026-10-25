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

/// Groups marks drawn on one screen with nothing clicked between them.
pub(super) fn group_marks(
    frames: &Frames<'_>,
    marks: &[Mark],
    actions: &[Moment],
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
            .map(|mark| mark.strokes.len())
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
                && ink(group).saturating_add(mark.strokes.len()) <= GROUP_STROKES
                && !acted_between(previous.span.end(), mark.span.start())
                && spread(group.clone(), index) < SAME_SCREEN
        });
        match groups.last_mut() {
            Some(group) if joins => group.end = index.saturating_add(1),
            _ => groups.push(index..index.saturating_add(1)),
        }
    }

    groups
}
