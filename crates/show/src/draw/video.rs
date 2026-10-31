//! Progressive saved annotations use only observed points, completed actions, and matching spaces.

mod labels;

use std::{fmt, time::Duration};

use serde::Serialize;
use tiny_skia::{PixmapPaint, Transform};

use super::{RECENT_INK, RIPPLE_REACH, closed_curve, fill, label, ripple};
use crate::{
    Action, Error, Image, Mark, Moment, Point, Recording, Rect, Shape, Stroke, Typeface, ink,
    locate::PixelRect, recording::Spaces,
};

/// The fitted image and the display rectangle captured in one presented video sample.
#[derive(Clone, Copy, Debug)]
pub struct VideoViewport {
    /// Content bounds in the full encoded canvas; letterboxing remains untouched.
    pub content: PixelRect,
    /// Display edges normalized to the capture's original display coordinate grid.
    pub source: Rect,
}

/// Counts drawn primitives and eligible primitives omitted for unknown or out-of-frame geometry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AnnotationReport {
    /// Visible strokes, numbered labels, and completed action ripples.
    pub drawn: usize,
    /// Eligible primitives outside the frame or without compatible coordinate and timing proofs.
    pub omitted: usize,
}

/// An immutable annotation snapshot without narration, summary text, images, or I/O.
pub struct Annotations {
    duration: Moment,
    strokes: Vec<Stroke>,
    actions: Vec<Action>,
    spaces: Spaces,
    labels: Vec<labels::Label>,
}

impl fmt::Debug for Annotations {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("Annotations")
            .field("duration", &self.duration)
            .field("strokes", &self.strokes.len())
            .field("labels", &self.labels.len())
            .field("actions", &self.actions.len())
            .finish_non_exhaustive()
    }
}

impl Annotations {
    /// Copies only the annotation data of a validated Show for repeated frame rendering.
    ///
    /// # Errors
    /// Rejects invalid normalized geometry, times outside the recording, and unbound proofs.
    pub fn new(recording: &Recording, marks: &[Mark]) -> Result<Self, Error> {
        let point = |point: Point| {
            point.is_finite() && (0.0..=1.0).contains(&point.x) && (0.0..=1.0).contains(&point.y)
        };
        let valid = recording.spaces.valid(
            recording.strokes.len(),
            recording.pins.len(),
            recording.actions.len(),
            recording.duration,
        ) && recording.strokes.iter().all(|stroke| {
            stroke
                .points()
                .iter()
                .all(|ink| point(ink.point) && ink.at <= recording.duration)
        }) && recording
            .pins
            .iter()
            .all(|pin| point(pin.point) && pin.at <= recording.duration)
            && recording.actions.iter().all(|action| {
                action.span().end() <= recording.duration
                    && match action {
                        Action::Click { point: at, .. } => at.is_finite(),
                        Action::Drag { from, to, .. } => from.is_finite() && to.is_finite(),
                    }
            })
            && marks.iter().all(|mark| {
                mark.span.end() <= recording.duration
                    && mark.strokes.start <= mark.strokes.end
                    && mark.strokes.end <= recording.strokes.len()
                    && point(Point::new(mark.bounds.left, mark.bounds.top))
                    && point(Point::new(mark.bounds.right, mark.bounds.bottom))
                    && (matches!(mark.shape, Shape::Pin { .. })
                        || recording
                            .strokes
                            .get(mark.strokes.clone())
                            .is_some_and(|strokes| {
                                !strokes.is_empty()
                                    && strokes.iter().all(|stroke| {
                                        mark.span.start() <= stroke.span().start()
                                            && stroke.span().end() <= mark.span.end()
                                    })
                            }))
                    && match mark.shape {
                        Shape::Pin { point: at } => point(at),
                        Shape::Line { from, to } | Shape::Arrow { from, to } => {
                            point(from) && point(to)
                        },
                        _ => true,
                    }
            });
        if !valid {
            return Err(Error::evidence(
                "Saved annotation geometry or timing is invalid",
            ));
        }
        Ok(Self {
            duration: recording.duration,
            strokes: recording.strokes.clone(),
            actions: recording.actions.clone(),
            spaces: recording.spaces.clone(),
            labels: labels::resolve(recording, marks),
        })
    }

    /// Paints at the actual decoded session timestamp. Observations have millisecond resolution.
    /// Only the eligible point prefix shapes an outline; labels wait for complete marks, and
    /// actions appear after release. Cut segments retain ink together until 1.2 seconds after
    /// their final stored point. This is a saved-view policy, not a replay of live overlay fades.
    ///
    /// # Errors
    /// Rejects a time beyond annotation coverage or a viewport outside the supplied image.
    /// Unknown or incompatible primitive geometry is counted and omitted. Cancellation returns
    /// `None` between primitive batches without changing the supplied image.
    pub fn paint(
        &self,
        image: &mut Image,
        at: Duration,
        viewport: VideoViewport,
        typeface: &Typeface,
        cancelled: impl Fn() -> bool,
    ) -> Result<Option<AnnotationReport>, Error> {
        if cancelled() {
            return Ok(None);
        }
        if at > self.duration.since(Moment::START)
            || !crate::recording::spaces::valid(viewport.source)
        {
            return Err(Error::evidence(
                "Video sample has no compatible annotation coverage",
            ));
        }
        let PixelRect {
            left,
            top,
            width,
            height,
        } = viewport.content;
        let mut content = image.cropped(left, top, width, height)?;
        let (w, h) = (width as f32, height as f32);
        let mut report = AnnotationReport::default();
        let mut shown = vec![false; self.strokes.len()];
        let mut canvas = content.as_mut();
        let strokes = self
            .strokes
            .chunk_by(|one, two| one.last() == two.first())
            .flat_map(|chain| {
                let end = chain.last().map_or(Moment::START, |last| last.span().end());
                chain.iter().enumerate().map(move |(index, stroke)| {
                    let start = if index == 0 {
                        Some(stroke.first())
                    } else {
                        stroke.points().get(1).copied()
                    };
                    (stroke, end, start)
                })
            });
        for (index, (stroke, end, start)) in strokes.enumerate() {
            if cancelled() {
                return Ok(None);
            }
            if start.is_none_or(|start| start.at.since(Moment::START) > at)
                || at.saturating_sub(end.since(Moment::START)) > RECENT_INK
            {
                continue;
            }
            if self
                .spaces
                .strokes
                .get(index)
                .copied()
                .flatten()
                .is_none_or(|space| space.part != viewport.source)
            {
                report.omitted = report.omitted.saturating_add(1);
                continue;
            }
            let points: Vec<_> = stroke
                .points()
                .iter()
                .take_while(|ink| ink.at.since(Moment::START) <= at)
                .map(|ink| ink.point.scaled(w, h))
                .collect();
            fill(
                &mut canvas,
                closed_curve(&ink::outline(&points, ink::pen(w, h))),
                ink::COLOR,
            );
            if let Some(shown) = shown.get_mut(index) {
                *shown = true;
            }
            report.drawn = report.drawn.saturating_add(1);
        }
        let visible: Vec<_> = std::iter::once(0)
            .chain(shown.iter().scan(0_usize, |count, shown| {
                *count = count.saturating_add(usize::from(*shown));
                Some(*count)
            }))
            .collect();
        for bound in &self.labels {
            if cancelled() {
                return Ok(None);
            }
            match bound.placement(at, &visible, viewport.source) {
                labels::Placement::Hidden => continue,
                labels::Placement::Omitted => {
                    report.omitted = report.omitted.saturating_add(1);
                    continue;
                },
                labels::Placement::Visible => {},
            }
            let mark = &bound.mark;
            let radius = ink::label_radius(w, h);
            label(
                &mut canvas,
                mark,
                mark.label_center(w, h, radius),
                radius,
                typeface,
            );
            report.drawn = report.drawn.saturating_add(1);
        }
        for (index, action) in self.actions.iter().enumerate() {
            if cancelled() {
                return Ok(None);
            }
            let completed = action.span().end().since(Moment::START);
            if completed > at || at.saturating_sub(completed) > RIPPLE_REACH {
                continue;
            }
            let endpoint = action.point();
            if !(0.0..=1.0).contains(&endpoint.x)
                || !(0.0..=1.0).contains(&endpoint.y)
                || self
                    .spaces
                    .actions
                    .get(index)
                    .copied()
                    .flatten()
                    .is_none_or(|space| space.part != viewport.source)
            {
                report.omitted = report.omitted.saturating_add(1);
                continue;
            }
            ripple(&mut canvas, action.point().scaled(w, h), ink::detail(w, h));
            report.drawn = report.drawn.saturating_add(1);
        }
        let x = i32::try_from(left)
            .map_err(|_outside| Error::evidence("Video content is outside its canvas"))?;
        let y = i32::try_from(top)
            .map_err(|_outside| Error::evidence("Video content is outside its canvas"))?;
        if cancelled() {
            return Ok(None);
        }
        image.as_mut().draw_pixmap(
            x,
            y,
            content.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        Ok(Some(report))
    }
}

#[cfg(test)]
mod tests;
