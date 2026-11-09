mod content;

use content::Content;

use crate::{
    Image, Keyframe, Mark, Moment, Point, Recording, Rect, Selection, Size,
    draw::Shown,
    recording::Action,
    signature::{Changed, Sample},
};

const CONTEXT: f32 = 0.28;

const LEAST_WIDTH: f32 = 320.0;
const LEAST_HEIGHT: f32 = 200.0;

const MOST_ZOOM: f32 = 4.0;

const WORTH_CROPPING: f32 = 0.85;

const SHARED: f32 = 0.6;

const CAUSE_REACH: f32 = 40.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    Automatic,
    Chosen,
}

impl Selection {
    #[must_use]
    pub fn framed(mut self, recording: &Recording, samples: &[Sample]) -> Self {
        let changes: Vec<Vec<Rect>> = self
            .keyframes
            .iter()
            .scan(Moment::START, |since, keyframe| {
                let changed = changed_between(samples, *since, keyframe.at);
                *since = keyframe.at;
                Some(changed.regions())
            })
            .collect();
        let crops = plan(recording, &self.keyframes, &self.marks, &changes);
        for (keyframe, crop) in self.keyframes.iter_mut().zip(crops) {
            keyframe.crop = crop;
        }
        self
    }
}

fn changed_between(samples: &[Sample], since: Moment, until: Moment) -> Changed {
    let mut changed = Changed::none();
    for [before, after] in samples.array_windows::<2>() {
        if after.at > since && after.at <= until {
            changed.include(&before.signature.changed(&after.signature));
        }
    }
    changed
}

pub(crate) fn plan(
    recording: &Recording,
    keyframes: &[Keyframe],
    marks: &[Mark],
    changes: &[Vec<Rect>],
) -> Vec<Option<Rect>> {
    let screen = Screen::of(recording.surface);
    let found: Vec<Option<Rect>> = keyframes
        .iter()
        .enumerate()
        .map(|(position, keyframe)| {
            let (start, end) = slice(keyframes, position, recording.duration);
            let touched: Vec<Point> = recording
                .actions
                .iter()
                .filter(|action| action.span().end() >= start && action.span().start() <= end)
                .flat_map(|action| match *action {
                    Action::Click { point, .. } => vec![point],
                    Action::Drag { from, to, .. } => vec![from, to],
                })
                .filter(|point| point.is_finite())
                .collect();
            let shown = Shown::on(keyframe, recording, marks);
            let deliberate = Rect::bounding(
                shown
                    .points()
                    .filter(|point| point.is_finite())
                    .chain(touched.iter().copied()),
            );
            let regions = changes.get(position).map_or(&[][..], Vec::as_slice);
            let caused = regions
                .iter()
                .copied()
                .filter(|region| {
                    let reach = screen.pad(*region, CAUSE_REACH);
                    touched.iter().any(|&point| reach.contains(point))
                })
                .reduce(Rect::union);
            let activity = match (deliberate, caused) {
                (Some(deliberate), Some(caused)) => Some(deliberate.union(caused)),
                (deliberate, caused) => deliberate.or(caused),
            }
            .or_else(|| regions.iter().copied().reduce(Rect::union))?;
            Some(screen.finish(screen.margin(activity)))
        })
        .collect();
    let mut crops: Vec<Rect> = found
        .iter()
        .enumerate()
        .map(|(position, crop)| {
            crop.or_else(|| {
                let earlier = found.get(..position).unwrap_or_default().iter().rev();
                let later = found.get(position..).unwrap_or_default().iter();
                earlier.chain(later).flatten().next().copied()
            })
            .unwrap_or(Rect::UNIT)
        })
        .collect();
    share_steady_views(&mut crops, screen);
    crops
        .into_iter()
        .map(|crop| (crop != Rect::UNIT).then_some(crop))
        .collect()
}

pub(crate) fn fit(crop: Rect, drawn: Option<Rect>, frame: &Image, surface: Size) -> Option<Rect> {
    let screen = Screen::of(surface);
    let crop = match drawn {
        Some(drawn) if !crop.encloses(drawn) => screen.finish(crop.union(drawn)),
        _ => crop,
    };
    let crop = (crop != Rect::UNIT).then(|| Content::of(frame, surface).snap(crop, screen))?;
    (crop.area() < WORTH_CROPPING).then_some(crop)
}

fn slice(keyframes: &[Keyframe], position: usize, duration: Moment) -> (Moment, Moment) {
    let at = keyframes
        .get(position)
        .map_or(Moment::START, |keyframe| keyframe.at);
    let halfway = |other: Option<&Keyframe>, otherwise: Moment| {
        other.map_or(otherwise, |other| {
            Moment::from_millis(at.as_millis().midpoint(other.at.as_millis()))
        })
    };
    let previous = position
        .checked_sub(1)
        .and_then(|before| keyframes.get(before));
    let next = keyframes.get(position.saturating_add(1));
    (
        halfway(previous, Moment::START),
        halfway(next, duration.max(at)),
    )
}

fn share_steady_views(crops: &mut [Rect], screen: Screen) {
    let mut start = 0;
    while start < crops.len() {
        let mut end = start.saturating_add(1);
        let mut joined = crops.get(start).copied().unwrap_or(Rect::UNIT);
        while let Some(&next) = crops.get(end) {
            if joined.overlap(next) < SHARED {
                break;
            }
            joined = joined.union(next);
            end = end.saturating_add(1);
        }
        if end.saturating_sub(start) > 1 {
            let shared = screen.finish(joined);
            for crop in crops.get_mut(start..end).unwrap_or_default() {
                *crop = shared;
            }
        }
        start = end;
    }
}

#[derive(Clone, Copy)]
struct Screen {
    width: f32,
    height: f32,
}

impl Screen {
    fn of(surface: Size) -> Self {
        Self {
            width: surface.width.max(1.0),
            height: surface.height.max(1.0),
        }
    }

    fn pad(self, rect: Rect, units: f32) -> Rect {
        let (across, down) = (units / self.width, units / self.height);
        Rect {
            left: rect.left - across,
            top: rect.top - down,
            right: rect.right + across,
            bottom: rect.bottom + down,
        }
    }

    fn margin(self, rect: Rect) -> Rect {
        self.pad(rect, CONTEXT * (self.width * self.height).sqrt())
    }

    fn finish(self, rect: Rect) -> Rect {
        let center = rect.center();
        let mut width = (rect.width().max(1e-4) * self.width).max(LEAST_WIDTH.min(self.width));
        let mut height = (rect.height().max(1e-4) * self.height).max(LEAST_HEIGHT.min(self.height));
        let cover = (width / self.width).max(height / self.height);
        let need = MOST_ZOOM.recip();
        if cover < need {
            width *= need / cover;
            height *= need / cover;
        }
        let (width, height) = (
            (width / self.width).min(1.0),
            (height / self.height).min(1.0),
        );
        let keep = |center: f32, half: f32| center.max(half).min(1.0 - half);
        let center = Point::new(keep(center.x, width / 2.0), keep(center.y, height / 2.0));
        let shaped = Rect {
            left: (center.x - width / 2.0).max(0.0),
            top: (center.y - height / 2.0).max(0.0),
            right: (center.x + width / 2.0).min(1.0),
            bottom: (center.y + height / 2.0).min(1.0),
        };
        if shaped.area() >= WORTH_CROPPING || !shaped.is_finite() {
            Rect::UNIT
        } else {
            shaped
        }
    }
}

#[cfg(test)]
mod tests;
