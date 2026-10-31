//! Label bindings resolve pin identity once and require observed, completed, compatible geometry.

use std::{collections::BTreeMap, time::Duration};

use super::RECENT_INK;
use crate::{Mark, Moment, Point, Recording, Rect, Shape};

pub(super) struct Label {
    pub mark: Mark,
    anchor: Anchor,
}

enum Anchor {
    Ink(Option<Rect>),
    Pin(Option<(Rect, Moment)>),
}

pub(super) enum Placement {
    Hidden,
    Omitted,
    Visible,
}

pub(super) fn resolve(recording: &Recording, marks: &[Mark]) -> Vec<Label> {
    let key = |at: Moment, point: Point| {
        let bits = |value: f32| if value == 0.0 { 0 } else { value.to_bits() };
        (at.as_millis(), bits(point.x), bits(point.y))
    };
    let mut pins = BTreeMap::new();
    for (index, pin) in recording.pins.iter().enumerate() {
        let proof = recording.spaces.pins.get(index).copied().flatten();
        pins.entry(key(pin.at, pin.point))
            .and_modify(|proof| *proof = None)
            .or_insert(proof);
    }
    marks
        .iter()
        .map(|mark| {
            let anchor = if let Shape::Pin { point } = mark.shape {
                Anchor::Pin(
                    pins.get(&key(mark.span.start(), point))
                        .copied()
                        .flatten()
                        .and_then(|proof| proof.observed.map(|at| (proof.space.part, at))),
                )
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
            Anchor::Pin(Some((part, observed))) => {
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
