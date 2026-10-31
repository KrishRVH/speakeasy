use super::*;

mod integrity;
use crate::{InkPoint, Size, Span, Tracker, recording::spaces::Space, typeface::tests::typeface};

const WHOLE: Rect = Rect {
    left: 0.0,
    top: 0.0,
    right: 1.0,
    bottom: 1.0,
};
const VIEW: VideoViewport = VideoViewport {
    content: PixelRect {
        left: 10,
        top: 20,
        width: 100,
        height: 60,
    },
    source: WHOLE,
};

fn ms(value: u64) -> Moment {
    Moment::from_millis(value)
}

fn blank() -> Image {
    Image::from_bgra(120, 100, [17, 23, 31, 255].repeat(12_000)).unwrap()
}

fn painted(scene: &Annotations, at: Duration) -> (Vec<u8>, AnnotationReport) {
    let mut image = blank();
    let report = scene
        .paint(&mut image, at, VIEW, &typeface(), || false)
        .unwrap()
        .unwrap();
    (image.into_bgra(), report)
}

fn stroke(points: &[(f32, f32, u64)]) -> Stroke {
    Stroke::new(
        points
            .iter()
            .map(|&(x, y, at)| InkPoint::from((x, y, ms(at))))
            .collect(),
    )
    .unwrap()
}

fn recording(strokes: Vec<Stroke>) -> Recording {
    Recording {
        duration: ms(3000),
        spaces: Spaces {
            strokes: vec![
                Some(Space {
                    generation: 1,
                    part: WHOLE
                });
                strokes.len()
            ],
            ..Spaces::default()
        },
        strokes,
        ..Recording::default()
    }
}

#[test]
fn future_points_do_not_shape_the_present_and_letterboxing_is_unchanged() {
    let full = recording(vec![stroke(&[
        (0.2, 0.5, 100),
        (0.5, 0.5, 150),
        (0.8, 0.8, 200),
    ])]);
    let prefix = recording(vec![stroke(&[(0.2, 0.5, 100)])]);
    let scene = Annotations::new(&full, &[]).unwrap();
    let at = Duration::from_micros(149_999);
    assert_eq!(
        painted(&scene, at),
        painted(&Annotations::new(&prefix, &[]).unwrap(), at)
    );
    let (pixels, report) = painted(&scene, Duration::from_millis(150));
    assert_eq!(
        report,
        AnnotationReport {
            drawn: 1,
            omitted: 0
        }
    );
    assert_ne!(pixels, painted(&scene, at).0);
    let original = blank().into_bgra();
    for (index, (actual, original)) in pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(original.as_chunks::<4>().0)
        .enumerate()
    {
        let (x, y) = (index % 120, index / 120);
        if !(10..110).contains(&x) || !(20..80).contains(&y) {
            assert_eq!(actual, original);
        }
    }
}

#[test]
fn continuous_cut_segments_expire_together() {
    let recording = recording(vec![
        stroke(&[(0.1, 0.5, 100), (0.4, 0.5, 200)]),
        stroke(&[(0.4, 0.5, 200), (0.6, 0.5, 300), (0.8, 0.5, 1600)]),
    ]);
    let scene = Annotations::new(&recording, &[]).unwrap();
    let (pixels, report) = painted(&scene, Duration::from_millis(1500));
    assert_eq!(report.drawn, 2);
    let pixel = (50 * 120 + 30) * 4;
    assert_ne!(&pixels[pixel..pixel + 4], &[17, 23, 31, 255]);
    assert_eq!(
        painted(&scene, Duration::from_millis(2801)),
        (blank().into_bgra(), AnnotationReport::default())
    );
}

#[test]
fn labels_wait_for_the_complete_mark() {
    let mut tracker = Tracker::new(Size {
        width: 1000.0,
        height: 1000.0,
    });
    assert!(tracker.locate(Some(WHOLE)));
    tracker.observe(ms(100), Point::new(0.2, 0.5), true, false);
    tracker.observe(ms(200), Point::new(0.8, 0.5), true, false);
    tracker.observe(ms(210), Point::new(0.8, 0.5), false, false);
    let marks = tracker.marks();
    let recording = tracker.finish(ms(1000));
    let scene = Annotations::new(&recording, &marks).unwrap();
    let ink = Annotations::new(&recording, &[]).unwrap();
    assert_eq!(
        painted(&scene, Duration::from_millis(150)),
        painted(&ink, Duration::from_millis(150))
    );
    assert_eq!(painted(&scene, Duration::from_millis(200)).1.drawn, 2);
}

#[test]
fn delayed_pins_wait_for_observation_and_legacy_timing_stays_unknown() {
    let mut tracker = Tracker::new(Size {
        width: 1000.0,
        height: 1000.0,
    });
    assert!(tracker.locate(Some(WHOLE)));
    tracker.observe(ms(120), Point::new(0.5, 0.5), false, false);
    assert!(tracker.pin(Span::new(ms(80), ms(100))));
    let marks = tracker.marks();
    let mut recording = tracker.finish(ms(3000));
    let scene = Annotations::new(&recording, &marks).unwrap();
    assert_eq!(
        painted(&scene, Duration::from_micros(119_999)),
        (blank().into_bgra(), AnnotationReport::default())
    );
    assert_eq!(painted(&scene, Duration::from_millis(120)).1.drawn, 1);
    assert_eq!(painted(&scene, Duration::from_millis(1320)).1.drawn, 1);
    assert_eq!(
        painted(
            &scene,
            Duration::from_millis(1320) + Duration::from_nanos(1)
        )
        .1
        .drawn,
        0
    );
    recording.spaces.pins[0].as_mut().unwrap().observed = None;
    let legacy = Annotations::new(&recording, &marks).unwrap();
    assert_eq!(
        painted(&legacy, Duration::from_millis(2000)).1,
        AnnotationReport {
            drawn: 0,
            omitted: 1
        }
    );
}

#[test]
fn actions_wait_for_release_and_incompatible_coordinates_are_omitted() {
    let mut tracker = Tracker::new(Size {
        width: 1000.0,
        height: 1000.0,
    });
    assert!(tracker.locate(Some(WHOLE)));
    tracker.observe(ms(100), Point::new(0.2, 0.5), false, true);
    tracker.observe(ms(200), Point::new(0.8, 0.5), false, false);
    let mut recording = tracker.finish(ms(1000));
    let scene = Annotations::new(&recording, &[]).unwrap();
    assert_eq!(
        painted(&scene, Duration::from_micros(199_999)),
        (blank().into_bgra(), AnnotationReport::default())
    );
    assert_eq!(painted(&scene, Duration::from_millis(200)).1.drawn, 1);
    assert_eq!(painted(&scene, Duration::from_millis(801)).1.drawn, 0);
    recording.spaces.actions[0].as_mut().unwrap().part.right = 0.5;
    let mixed = Annotations::new(&recording, &[]).unwrap();
    assert_eq!(
        painted(&mixed, Duration::from_millis(200)),
        (
            blank().into_bgra(),
            AnnotationReport {
                drawn: 0,
                omitted: 1
            }
        )
    );
    assert!(
        scene
            .paint(
                &mut blank(),
                Duration::from_nanos(1_000_000_001),
                VIEW,
                &typeface(),
                || false
            )
            .is_err()
    );
}

#[test]
fn releasing_outside_the_target_does_not_disable_valid_ink() {
    let mut tracker = Tracker::new(Size {
        width: 1000.0,
        height: 1000.0,
    });
    assert!(tracker.locate(Some(WHOLE)));
    tracker.observe(ms(10), Point::new(0.2, 0.5), true, false);
    tracker.observe(ms(20), Point::new(0.8, 0.5), true, false);
    tracker.observe(ms(30), Point::new(0.8, 0.5), false, false);
    tracker.observe(ms(100), Point::new(0.5, 0.5), false, true);
    tracker.observe(ms(200), Point::new(1.2, 0.5), false, false);
    let recording = tracker.finish(ms(1000));
    let scene = Annotations::new(&recording, &[]).unwrap();
    assert_eq!(
        painted(&scene, Duration::from_millis(200)).1,
        AnnotationReport {
            drawn: 1,
            omitted: 1
        }
    );
}
