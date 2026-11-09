mod content;

use std::iter;

use content::Content;

use crate::{
    Image, Keyframe, Layer, Layout, Mark, Moment, Point, Recording, Rect, Selection, Size, Window,
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

const SUBJECT_SHARE: f32 = 0.8;

const MOST_SUBJECTS: usize = 2;

const WHOLE_WINDOW: f32 = 0.9;

const WINDOW_PAD: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Plan {
    pub(crate) crop: Rect,
    pub(crate) within: Rect,
}

impl Plan {
    const WHOLE: Self = Self {
        crop: Rect::UNIT,
        within: Rect::UNIT,
    };

    const fn anywhere(crop: Rect) -> Self {
        Self {
            crop,
            within: Rect::UNIT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    Automatic,
    Chosen,
}

impl Selection {
    #[must_use]
    pub fn framed(mut self, recording: &Recording, samples: &[Sample], layouts: &[Layout]) -> Self {
        let changes: Vec<Vec<Rect>> = self
            .keyframes
            .iter()
            .scan(Moment::START, |since, keyframe| {
                let changed = changed_between(samples, *since, keyframe.at);
                *since = keyframe.at;
                Some(changed.regions())
            })
            .collect();
        self.plans = plan(recording, &self.keyframes, &self.marks, &changes, layouts);
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
    layouts: &[Layout],
) -> Vec<Option<Plan>> {
    let screen = Screen::of(recording.surface);
    let found: Vec<Option<Plan>> = keyframes
        .iter()
        .enumerate()
        .map(|(position, keyframe)| {
            let (start, end) = slice(keyframes, position, recording.duration);
            let touched: Vec<Vec<(Point, Moment)>> = recording
                .actions
                .iter()
                .filter(|action| action.span().end() >= start && action.span().start() <= end)
                .map(|action| match *action {
                    Action::Click { at, point } => vec![(point, at)],
                    Action::Drag { span, from, to } => vec![(from, span.start()), (to, span.end())],
                })
                .collect();
            let shown = Shown::on(keyframe, recording, marks);
            let drawn = shown
                .gestures()
                .map(|gesture| gesture.iter().map(|&point| (point, keyframe.at)).collect());
            let votes: Vec<Vote> = drawn
                .chain(touched.iter().cloned())
                .flat_map(Vote::shared)
                .collect();
            let regions = changes.get(position).map_or(&[][..], Vec::as_slice);
            let caused = regions
                .iter()
                .copied()
                .filter(|region| {
                    let reach = screen.pad(*region, CAUSE_REACH);
                    touched
                        .iter()
                        .flatten()
                        .any(|&(point, _)| reach.contains(point))
                })
                .reduce(Rect::union);
            let worked = Worked {
                pointed: Rect::bounding(
                    shown
                        .points()
                        .chain(touched.iter().flatten().map(|&(point, _)| point))
                        .filter(|point| point.is_finite()),
                ),
                caused,
            };
            let now = Layout::at(layouts, keyframe.at);
            let windows = if votes.is_empty() {
                now.and_then(|layout| unpointed(layout, regions))
            } else {
                subject(&votes, layouts, now)
            };
            match windows {
                Some(windows) => Some(screen.frame(windows, worked)),
                None => worked
                    .input()
                    .or_else(|| regions.iter().copied().reduce(Rect::union))
                    .map(|activity| Plan::anywhere(screen.finish(screen.margin(activity)))),
            }
        })
        .collect();
    let mut plans: Vec<Plan> = found
        .iter()
        .enumerate()
        .map(|(position, plan)| {
            plan.or_else(|| {
                let earlier = found.get(..position).unwrap_or_default().iter().rev();
                let later = found.get(position..).unwrap_or_default().iter();
                earlier.chain(later).flatten().next().copied()
            })
            .unwrap_or(Plan::WHOLE)
        })
        .collect();
    share_steady_views(&mut plans, screen);
    plans
        .into_iter()
        .map(|plan| (plan.crop != Rect::UNIT).then_some(plan))
        .collect()
}

#[derive(Clone, Copy)]
struct Worked {
    pointed: Option<Rect>,
    caused: Option<Rect>,
}

impl Worked {
    fn input(self) -> Option<Rect> {
        match (self.pointed, self.caused) {
            (Some(pointed), Some(caused)) => Some(pointed.union(caused)),
            (pointed, caused) => pointed.or(caused),
        }
    }
}

#[derive(Clone, Copy)]
struct Vote {
    point: Point,
    at: Moment,
    weight: f32,
}

impl Vote {
    fn shared(gesture: Vec<(Point, Moment)>) -> impl Iterator<Item = Self> {
        let finite: Vec<(Point, Moment)> = gesture
            .into_iter()
            .filter(|(point, _)| point.is_finite())
            .collect();
        let weight = (finite.len() as f32).recip();
        finite
            .into_iter()
            .map(move |(point, at)| Self { point, at, weight })
    }
}

fn subject(votes: &[Vote], layouts: &[Layout], now: Option<&Layout>) -> Option<Rect> {
    let mut held: Vec<(Window, f32)> = Vec::new();
    for vote in votes {
        if let Some(&window) =
            Layout::at(layouts, vote.at).and_then(|layout| layout.owner(vote.point))
        {
            count(&mut held, window, vote.weight);
        }
    }
    held.sort_by(|(_, one), (_, other)| other.total_cmp(one));
    let needed = SUBJECT_SHARE * votes.iter().map(|vote| vote.weight).sum::<f32>();
    let mut chosen = Vec::new();
    let mut counted = 0.0;
    for &(window, weight) in held.iter().take(MOST_SUBJECTS) {
        if counted >= needed {
            break;
        }
        counted += weight;
        chosen.push(window);
    }
    if counted < needed {
        return None;
    }
    chosen
        .iter()
        .map(|window| {
            let current = now
                .and_then(|layout| layout.find(window.id))
                .unwrap_or(window);
            with_its_app(now, current)
        })
        .reduce(Rect::union)
}

fn unpointed(layout: &Layout, regions: &[Rect]) -> Option<Rect> {
    let front = layout.front()?;
    let mut changed: Vec<(Window, f32)> = Vec::new();
    for region in regions {
        if let Some(&window) = layout
            .owner(region.center())
            .filter(|window| window.layer == Layer::Normal)
        {
            count(&mut changed, window, region.area());
        }
    }
    let busiest = changed
        .iter()
        .max_by(|(_, one), (_, other)| one.total_cmp(other))
        .map(|(window, _)| window)
        .filter(|window| window.id != front.id);
    iter::once(front)
        .chain(busiest)
        .map(|window| with_its_app(Some(layout), window))
        .reduce(Rect::union)
}

fn count(held: &mut Vec<(Window, f32)>, window: Window, weight: f32) {
    match held.iter_mut().find(|(other, _)| other.id == window.id) {
        Some((_, total)) => *total += weight,
        None => held.push((window, weight)),
    }
}

fn with_its_app(now: Option<&Layout>, window: &Window) -> Rect {
    now.into_iter()
        .flat_map(|layout| layout.ahead_of(window))
        .fold(window.bounds, |framed, ahead| framed.union(ahead.bounds))
}

pub(crate) fn fit(plan: Plan, drawn: Option<Rect>, frame: &Image, surface: Size) -> Option<Rect> {
    let screen = Screen::of(surface);
    let Plan { crop, within } = match drawn {
        Some(drawn) if !plan.crop.encloses(drawn) => {
            let crop = screen.finish(plan.crop.union(drawn));
            Plan {
                crop,
                within: plan.within.union(crop),
            }
        },
        _ => plan,
    };
    let crop = match crop {
        Rect::UNIT => return None,
        settled if settled == within => settled,
        open => Content::of(frame, surface).snap(open, screen, within),
    };
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

fn share_steady_views(plans: &mut [Plan], screen: Screen) {
    let mut start = 0;
    while start < plans.len() {
        let mut end = start.saturating_add(1);
        let mut joined = plans.get(start).copied().unwrap_or(Plan::WHOLE);
        while let Some(&next) = plans.get(end) {
            if joined.crop.overlap(next.crop) < SHARED {
                break;
            }
            joined = Plan {
                crop: joined.crop.union(next.crop),
                within: joined.within.union(next.within),
            };
            end = end.saturating_add(1);
        }
        if end.saturating_sub(start) > 1 {
            let crop = screen.finish(joined.crop);
            let shared = Plan {
                crop,
                within: joined.within.union(crop),
            };
            for plan in plans.get_mut(start..end).unwrap_or_default() {
                *plan = shared;
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

    fn frame(self, windows: Rect, worked: Worked) -> Plan {
        let within = self.pad(windows, WINDOW_PAD).intersection(Rect::UNIT);
        if windows.area() < WHOLE_WINDOW {
            let framed = worked
                .pointed
                .map_or(windows, |pointed| windows.union(pointed));
            let crop = self.finish(self.pad(framed, WINDOW_PAD));
            return Plan { crop, within: crop };
        }
        let focus = worked
            .input()
            .map(|input| input.intersection(within))
            .filter(|inside| inside.area() > 0.0)
            .or(worked.pointed)
            .unwrap_or(windows);
        Plan {
            crop: self.finish_within(self.margin(focus), within),
            within,
        }
    }

    fn finish(self, rect: Rect) -> Rect {
        self.finish_within(rect, Rect::UNIT)
    }

    fn finish_within(self, rect: Rect, bounds: Rect) -> Rect {
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
            (width / self.width).min(bounds.width()),
            (height / self.height).min(bounds.height()),
        );
        let keep =
            |center: f32, half: f32, low: f32, high: f32| center.max(low + half).min(high - half);
        let center = Point::new(
            keep(center.x, width / 2.0, bounds.left, bounds.right),
            keep(center.y, height / 2.0, bounds.top, bounds.bottom),
        );
        let shaped = Rect {
            left: (center.x - width / 2.0).max(bounds.left),
            top: (center.y - height / 2.0).max(bounds.top),
            right: (center.x + width / 2.0).min(bounds.right),
            bottom: (center.y + height / 2.0).min(bounds.bottom),
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
