use proptest::prelude::*;

use super::*;
use crate::{Reason, recording::spaces};

mod corpus;
mod windows;

const SCREEN: Size = Size {
    width: 1_440.0,
    height: 900.0,
};

fn keyframe(index: usize, at: u64) -> Keyframe {
    Keyframe {
        index,
        at: Moment::from_millis(at),
        reason: Reason::Middle,
        event: String::new(),
        said: None,
        marks: 0..0,
        action: None,
        crop: None,
    }
}

fn clicked(surface: Size, clicks: &[(u64, Point)]) -> Recording {
    Recording {
        duration: Moment::from_millis(80_000),
        surface,
        actions: clicks
            .iter()
            .map(|&(at, point)| Action::Click {
                at: Moment::from_millis(at),
                point,
            })
            .collect(),
        ..Recording::default()
    }
}

fn crops(
    recording: &Recording,
    keyframes: &[Keyframe],
    changes: &[Vec<Rect>],
) -> Vec<Option<Rect>> {
    plan(recording, keyframes, &[], changes, &[])
        .into_iter()
        .map(|plan| plan.map(|plan| plan.crop))
        .collect()
}

fn part() -> impl Strategy<Value = Rect> {
    (0.0_f32..=1.0, 0.0_f32..=1.0, 0.0_f32..=1.0, 0.0_f32..=1.0).prop_map(|(x0, y0, x1, y1)| Rect {
        left: x0.min(x1),
        top: y0.min(y1),
        right: x0.max(x1),
        bottom: y0.max(y1),
    })
}

fn within(outer: Rect, inner: Rect) -> bool {
    outer
        .expanded_within(1e-5, Rect::from([-1.0, -1.0, 2.0, 2.0]))
        .encloses(inner)
}

#[test]
fn a_keyframe_without_activity_keeps_the_view_nearest_before_it() {
    let (first, second) = (Point::new(0.2, 0.25), Point::new(0.85, 0.8));
    let recording = clicked(SCREEN, &[(20_000, first), (60_000, second)]);
    let keyframes = [
        keyframe(1, 0),
        keyframe(2, 20_000),
        keyframe(3, 40_000),
        keyframe(4, 60_000),
    ];
    let crops = crops(&recording, &keyframes, &[]);
    let [Some(opening), Some(near), Some(talk), Some(far)] = crops[..] else {
        panic!("Every keyframe is cropped: {crops:?}");
    };
    assert!(near.contains(first) && !near.contains(second), "{near:?}");
    assert!(far.contains(second) && !far.contains(first), "{far:?}");
    assert_eq!(opening, near);
    assert_eq!(talk, near);
}

#[test]
fn a_change_away_from_the_input_does_not_widen_the_view() {
    let click = Point::new(0.2, 0.25);
    let recording = clicked(SCREEN, &[(10_000, click)]);
    let keyframes = [keyframe(1, 10_000), keyframe(2, 40_000)];
    let opened = Rect::from([0.15, 0.2, 0.32, 0.4]);
    let ticking = Rect::from([0.9, 0.0, 0.98, 0.03]);
    let panel = Rect::from([0.6, 0.6, 0.9, 0.9]);
    let changes = [vec![opened, ticking], vec![panel]];
    let crops = crops(&recording, &keyframes, &changes);
    let [Some(working), Some(elsewhere)] = crops[..] else {
        panic!("Both keyframes are cropped: {crops:?}");
    };
    assert!(
        working.encloses(opened) && working.contains(click),
        "{working:?}"
    );
    assert!(!working.contains(ticking.center()), "{working:?}");
    assert!(elsewhere.encloses(panel), "{elsewhere:?}");
}

#[test]
fn a_keyframe_with_nothing_to_frame_keeps_the_whole_screen() {
    let keyframes = [keyframe(1, 0), keyframe(2, 30_000)];
    assert_eq!(crops(&clicked(SCREEN, &[]), &keyframes, &[]), [None, None]);
    let everywhere = clicked(
        SCREEN,
        &[(0, Point::new(0.0, 0.0)), (0, Point::new(1.0, 1.0))],
    );
    assert_eq!(crops(&everywhere, &keyframes[..1], &[]), [None]);
}

proptest! {
    #[test]
    fn every_crop_is_a_readable_part_of_the_screen_around_its_input(
        clicks in prop::collection::vec((0.0_f32..=1.0, 0.0_f32..=1.0), 1..12),
        width in 100.0_f32..4_000.0,
        height in 100.0_f32..3_000.0,
    ) {
        let surface = Size { width, height };
        let clicks: Vec<(u64, Point)> = clicks
            .into_iter()
            .zip((0_u64..).step_by(5_000))
            .map(|((x, y), at)| (at, Point::new(x, y)))
            .collect();
        let keyframes: Vec<Keyframe> = clicks
            .iter()
            .zip(1..)
            .map(|(&(at, _), index)| keyframe(index, at))
            .collect();
        let crops = crops(&clicked(surface, &clicks), &keyframes, &[]);
        prop_assert_eq!(crops.len(), keyframes.len());
        for (crop, &(_, point)) in crops.into_iter().zip(&clicks) {
            let Some(crop) = crop else { continue };
            prop_assert!(spaces::valid(crop), "{:?}", crop);
            prop_assert!(crop.area() < WORTH_CROPPING, "{:?}", crop);
            prop_assert!(crop.width() * width >= LEAST_WIDTH.min(width) - 0.01, "{:?}", crop);
            prop_assert!(crop.height() * height >= LEAST_HEIGHT.min(height) - 0.01, "{:?}", crop);
            prop_assert!(crop.width().max(crop.height()) >= MOST_ZOOM.recip() - 1e-5, "{:?}", crop);
            prop_assert!(within(crop, Rect::at(point)), "{:?} {:?}", crop, point);
        }
    }

    #[test]
    fn drawing_outside_a_crop_widens_it_to_hold_the_drawing(
        activity in part(),
        drawn in part(),
        width in 100.0_f32..4_000.0,
        height in 100.0_f32..3_000.0,
    ) {
        let surface = Size { width, height };
        let screen = Screen::of(surface);
        let crop = screen.finish(screen.margin(activity));
        prop_assume!(crop != Rect::UNIT);
        let blank = Image::filled(320, 200, [250; 3]).unwrap();
        let plan = Plan::anywhere(crop);
        if let Some(grown) = fit(plan, Some(drawn), &blank, surface) {
            prop_assert!(spaces::valid(grown), "{:?}", grown);
            prop_assert!(within(grown, crop), "{:?} {:?}", grown, crop);
            prop_assert!(within(grown, drawn), "{:?} {:?}", grown, drawn);
        }
        prop_assert_eq!(fit(plan, None, &blank, surface), Some(crop));
    }
}
