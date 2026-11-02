use std::time::Duration;

use super::RECENT_INK;
use crate::{
    Mark, Moment, Recording, Rect, Shape,
    recording::spaces::{PinSpace, Space},
};

pub(super) struct Label {
    pub mark: Mark,
    anchor: Anchor,
}

enum Anchor {
    Ink(Option<Rect>),
    Pin(Option<PinSpace>),
}

pub(super) enum Placement {
    Hidden,
    Omitted,
    Visible,
}

pub(super) fn resolve(recording: &Recording, marks: &[Mark]) -> Vec<Label> {
    marks
        .iter()
        .map(|mark| {
            let anchor = if let Shape::Pin { .. } = mark.shape {
                Anchor::Pin(recording.spaces.pin(mark, &recording.pins))
            } else {
                Anchor::Ink(recording.spaces.mark(mark, &recording.pins))
            };
            Label {
                mark: mark.clone(),
                anchor,
            }
        })
        .collect()
}

impl Label {
    pub(super) fn placement(&self, at: Duration, visible: &[usize], source: Rect) -> Placement {
        if self.mark.span.end().since(Moment::START) > at {
            return Placement::Hidden;
        }
        let part = match self.anchor {
            Anchor::Pin(None) => None,
            Anchor::Pin(Some(PinSpace {
                space: Space { part, .. },
                observed,
            })) => {
                let start = observed.max(self.mark.span.end()).since(Moment::START);
                if at < start || at.saturating_sub(start) > RECENT_INK {
                    return Placement::Hidden;
                }
                Some(part)
            },
            Anchor::Ink(part) => {
                let range = &self.mark.strokes;
                if visible
                    .get(range.start)
                    .zip(visible.get(range.end))
                    .is_none_or(|(start, end)| start >= end)
                {
                    return Placement::Hidden;
                }
                part
            },
        };
        if part == Some(source) {
            Placement::Visible
        } else {
            Placement::Omitted
        }
    }
}
