mod labels;

use std::{fmt, time::Duration};

use serde::Serialize;
use tiny_skia::{PixmapPaint, Transform};

use super::{RECENT_INK, RIPPLE_REACH, closed_curve, fill, label, ripple};
use crate::{
    Action, Error, Image, Mark, Moment, Point, Recording, Rect, Shape, Stroke, Typeface, ink,
    locate::PixelRect,
    recording::{
        Spaces,
        spaces::{self, Space},
    },
};

#[derive(Clone, Copy, Debug)]
pub struct VideoViewport {
    pub content: PixelRect,
    pub source: Rect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AnnotationReport {
    pub drawn: usize,
    pub omitted: usize,
}

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
    pub fn new(recording: &Recording, marks: &[Mark]) -> Result<Self, Error> {
        if !consistent(recording, marks) {
            return Err(Error::invalid(
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

    pub fn paint(
        &self,
        image: &mut Image,
        at: Duration,
        viewport: VideoViewport,
        typeface: &Typeface,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Option<AnnotationReport>, Error> {
        if cancelled() {
            return Ok(None);
        }
        if at > self.duration.since(Moment::START) || !spaces::valid(viewport.source) {
            return Err(Error::invalid(
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
        let (Ok(x), Ok(y)) = (i32::try_from(left), i32::try_from(top)) else {
            return Err(Error::invalid("Video content is outside its canvas"));
        };
        let (w, h) = (width as f32, height as f32);
        let located = |spaces: &[Option<Space>], index: usize| {
            spaces
                .get(index)
                .copied()
                .flatten()
                .is_some_and(|space| space.part == viewport.source)
        };
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
            if !located(&self.spaces.strokes, index) {
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
            if let Some(drawn) = shown.get_mut(index) {
                *drawn = true;
            }
            report.drawn = report.drawn.saturating_add(1);
        }
        let visible: Vec<_> = std::iter::once(0)
            .chain(shown.iter().scan(0_usize, |count, shown| {
                *count = count.saturating_add(usize::from(*shown));
                Some(*count)
            }))
            .collect();
        let radius = ink::label_radius(w, h);
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
            if !Rect::UNIT.contains(endpoint) || !located(&self.spaces.actions, index) {
                report.omitted = report.omitted.saturating_add(1);
                continue;
            }
            ripple(&mut canvas, endpoint.scaled(w, h), ink::detail(w, h));
            report.drawn = report.drawn.saturating_add(1);
        }
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

fn consistent(recording: &Recording, marks: &[Mark]) -> bool {
    let Recording {
        duration,
        strokes,
        pins,
        actions,
        ..
    } = recording;
    recording.spaces_valid()
        && strokes
            .iter()
            .flat_map(Stroke::points)
            .all(|ink| Rect::UNIT.contains(ink.point) && ink.at <= *duration)
        && pins
            .iter()
            .all(|pin| Rect::UNIT.contains(pin.point) && pin.at <= *duration)
        && actions.iter().all(|action| {
            action.span().end() <= *duration
                && match *action {
                    Action::Click { point, .. } => point.is_finite(),
                    Action::Drag { from, to, .. } => from.is_finite() && to.is_finite(),
                }
        })
        && marks.iter().all(|mark| {
            let drawn = match (mark.shape, strokes.get(mark.strokes.clone())) {
                (_, None) => false,
                (Shape::Pin { .. }, Some(_)) => true,
                (_, Some(members)) => {
                    !members.is_empty()
                        && members.iter().all(|stroke| {
                            mark.span.start() <= stroke.span().start()
                                && stroke.span().end() <= mark.span.end()
                        })
                },
            };
            let placed = match mark.shape {
                Shape::Pin { point } => Rect::UNIT.contains(point),
                Shape::Line { from, to } | Shape::Arrow { from, to } => {
                    Rect::UNIT.contains(from) && Rect::UNIT.contains(to)
                },
                Shape::Tap | Shape::Circle | Shape::Underline | Shape::Scribble => true,
            };
            let Rect {
                left,
                top,
                right,
                bottom,
            } = mark.bounds;
            mark.span.end() <= *duration
                && drawn
                && placed
                && Rect::UNIT.contains(Point::new(left, top))
                && Rect::UNIT.contains(Point::new(right, bottom))
        })
}

#[cfg(test)]
mod tests;
