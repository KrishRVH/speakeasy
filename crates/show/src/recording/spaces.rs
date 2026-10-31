//! Coordinate proofs follow the points that actually contribute to a stroke or pin.

use serde::{Deserialize, Serialize};

use crate::{Mark, Moment, Pin, Rect, Shape};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Space {
    pub generation: u64,
    pub part: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct PinSpace {
    #[serde(flatten)]
    pub space: Space,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<Moment>,
}

/// Coordinate spaces for retained strokes and pins. Missing legacy provenance proves nothing;
/// a mixed stroke has no space, and grouped strokes must share one uninterrupted generation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spaces {
    pub(crate) strokes: Vec<Option<Space>>,
    pub(crate) pins: Vec<Option<PinSpace>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) actions: Vec<Option<Space>>,
}

impl Spaces {
    /// Whether no provenance was recorded, as in older bundles.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.pins.is_empty() && self.actions.is_empty()
    }

    pub(crate) fn valid(&self, strokes: usize, pins: usize, actions: usize, end: Moment) -> bool {
        ((self.strokes.is_empty() && self.pins.is_empty())
            || (self.strokes.len() == strokes && self.pins.len() == pins))
            && (self.actions.is_empty() || self.actions.len() == actions)
            && self
                .strokes
                .iter()
                .chain(&self.actions)
                .flatten()
                .all(|space| valid(space.part))
            && self
                .pins
                .iter()
                .flatten()
                .all(|proof| valid(proof.space.part) && proof.observed.is_none_or(|at| at <= end))
    }

    pub(crate) fn mark(&self, mark: &Mark, pins: &[Pin]) -> Option<Rect> {
        if let Shape::Pin { point } = mark.shape {
            let mut matches = pins
                .iter()
                .enumerate()
                .filter(|(_, pin)| pin.at == mark.span.start() && pin.point == point);
            let (index, _) = matches.next()?;
            if matches.next().is_some() {
                return None;
            }
            return self.pins.get(index)?.map(|proof| proof.space.part);
        }
        let members = self.strokes.get(mark.strokes.clone())?;
        let first = (*members.first()?)?;
        members
            .iter()
            .all(|space| *space == Some(first))
            .then_some(first.part)
    }
}

#[cfg(test)]
mod tests;

pub(crate) fn valid(part: Rect) -> bool {
    [part.left, part.top, part.right, part.bottom]
        .into_iter()
        .all(|edge| edge.is_finite() && (0.0..=1.0).contains(&edge))
        && part.left < part.right
        && part.top < part.bottom
}
