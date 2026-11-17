mod labels;

use std::{fmt, iter, time::Duration};

use tiny_skia::{PixmapPaint, Transform};

use super::{RECENT_INK, RIPPLE_REACH, closed_curve, fill, label, ripple};
use crate::{
    Action, Error, Image, Mark, Moment, PixelRect, Recording, Rect, Shape, Stroke, Typeface, ink,
    recording::spaces::{self, Space},
};

#[derive(Clone, Copy, Debug)]
pub struct VideoViewport {
    pub content: PixelRect,
    pub source: Rect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnnotationReport {
    pub drawn: usize,
    pub omitted: usize,
}

pub struct Annotations {
    duration: Moment,
    strokes: Vec<Located<Trace>>,
    actions: Vec<Located<Action>>,
    labels: Vec<labels::Label>,
}

struct Located<T> {
    item: T,
    part: Option<Rect>,
}

impl<T> Located<T> {
    fn all(items: impl IntoIterator<Item = T>, spaces: &[Option<Space>]) -> Vec<Self> {
        let parts = spaces
            .iter()
            .map(|space| space.map(|space| space.part))
            .chain(iter::repeat(None));
        items
            .into_iter()
            .zip(parts)
            .map(|(item, part)| Self { item, part })
            .collect()
    }
}

struct Trace {
    stroke: Stroke,
    appears: Option<Moment>,
    lifted: Moment,
}

impl Trace {
    fn all(strokes: &[Stroke]) -> impl Iterator<Item = Self> {
        strokes
            .chunk_by(|one, two| one.last() == two.first())
            .flat_map(|movement| {
                let lifted = movement
                    .last()
                    .map_or(Moment::START, |last| last.span().end());
                movement
                    .iter()
                    .enumerate()
                    .map(move |(index, stroke)| Self {
                        stroke: stroke.clone(),
                        appears: if index == 0 {
                            Some(stroke.first().at)
                        } else {
                            stroke.points().get(1).map(|ink| ink.at)
                        },
                        lifted,
                    })
            })
    }
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
            strokes: Located::all(Trace::all(&recording.strokes), &recording.spaces.strokes),
            actions: Located::all(recording.actions.iter().copied(), &recording.spaces.actions),
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
        let now = Moment::from(at);
        let mut report = AnnotationReport::default();
        let mut shown = vec![false; self.strokes.len()];
        let mut canvas = content.as_mut();
        for (trace, drawn) in self.strokes.iter().zip(&mut shown) {
            if cancelled() {
                return Ok(None);
            }
            let Trace {
                stroke,
                appears,
                lifted,
            } = &trace.item;
            if appears.is_none_or(|appears| appears > now)
                || at.saturating_sub(lifted.since(Moment::START)) > RECENT_INK
            {
                continue;
            }
            if trace.part != Some(viewport.source) {
                report.omitted = report.omitted.saturating_add(1);
                continue;
            }
            let points: Vec<_> = stroke
                .path_until(now)
                .map(|point| point.scaled(w, h))
                .collect();
            fill(
                &mut canvas,
                closed_curve(&ink::outline(&points, ink::pen(w, h))),
                ink::COLOR,
            );
            *drawn = true;
            report.drawn = report.drawn.saturating_add(1);
        }
        let radius = ink::label_radius(w, h);
        for bound in &self.labels {
            if cancelled() {
                return Ok(None);
            }
            match bound.placement(at, &shown, viewport.source) {
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
        for action in &self.actions {
            if cancelled() {
                return Ok(None);
            }
            let completed = action.item.span().end().since(Moment::START);
            if completed > at || at.saturating_sub(completed) > RIPPLE_REACH {
                continue;
            }
            let endpoint = action.item.point();
            if !Rect::UNIT.contains(endpoint) || action.part != Some(viewport.source) {
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
            mark.span.end() <= *duration && drawn && placed && mark.bounds.is_within_unit()
        })
}

#[cfg(test)]
mod tests;
