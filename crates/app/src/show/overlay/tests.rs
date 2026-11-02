use std::ops::Range;

use speakeasy_show::{Moment, Rect, Span};

use super::*;

fn stroke(points: u8) -> Vec<Point> {
    (0..points)
        .map(|point| Point::new(f32::from(point) / 100.0, 0.5))
        .collect()
}

fn clock() -> impl Fn(u64) -> Instant {
    let start = Instant::now();
    move |millis| start.checked_add(Duration::from_millis(millis)).unwrap()
}

fn whole(sheet: &Sheet, now: Instant) -> bool {
    (sheet.opacity(now) - 1.0).abs() < f32::EPSILON
}

#[test]
fn a_copied_show_whose_ink_failed_still_says_what_it_lost() {
    let error = anyhow!("No display available");
    let copied = Snapshot {
        phase: Phase::Copied,
        show: true,
        message: "Show copied. Paste it into your coding agent.".into(),
        ..Snapshot::default()
    };
    let reported = |snapshot: &Snapshot| failure_after(snapshot, &error).map(|e| e.to_string());
    assert_eq!(
        reported(&copied).as_deref(),
        Some("Show copied, but its ink could not appear on screen: No display available")
    );
    let unnarrated = Snapshot {
        ending: Ending::WindowClosed,
        narration_lost: Some(speakeasy_show::NarrationLoss::AudioStopped),
        message: "Show copied without narration, and the recorded window closed.".into(),
        ..copied.clone()
    };
    assert_eq!(
        reported(&unnarrated).as_deref(),
        Some(
            "Show copied without narration, and the recorded window closed. Its ink could not \
             appear on screen: No display available"
        ),
        "The ink failure hid what the Show lost"
    );
    let cancelled = Snapshot {
        phase: Phase::Cancelled,
        ..copied
    };
    assert!(
        reported(&cancelled).is_none(),
        "A cancelled Show reported its ink"
    );
}

#[test]
fn ink_holds_while_drawing_then_fades_out_after_the_pen_lifts() {
    let at = clock();
    let mut sheet = Sheet::new(Part::WHOLE);
    assert!(sheet.update([], stroke(3), None, at(0)));
    assert!(sheet.update([stroke(3).into()], [], None, at(0)));
    assert!(whole(&sheet, at(600)));
    let midway = sheet.opacity(at(975));
    assert!(midway > 0.2 && midway < 0.3, "{midway}");
    assert!(sheet.faded(at(1_300)));
    assert!(sheet.update([], stroke(4), None, at(600)));
    assert!(
        whole(&sheet, at(5_000)),
        "Drawing again keeps every mark whole for as long as the stroke takes"
    );
}

#[test]
fn held_ink_draws_no_frames_until_it_fades() {
    let at = clock();
    let mut sheet = Sheet::new(Part::WHOLE);
    sheet.update([], stroke(3), None, at(0));
    assert_eq!(sheet.next_frame(at(0)), Redraw::OnChange);
    sheet.update([stroke(3).into()], [], None, at(0));
    let left = Duration::from_millis(50);
    assert_eq!(sheet.next_frame(at(600)), Redraw::After(left));
    assert_eq!(sheet.next_frame(at(975)), Redraw::Animate);
    assert_eq!(sheet.next_frame(at(1_300)), Redraw::OnChange);
}

#[test]
fn marks_drawn_before_the_fade_ends_stay_and_later_ink_starts_clean() {
    let at = clock();
    let mut sheet = Sheet::new(Part::WHOLE);
    sheet.update([], stroke(3), None, at(0));
    sheet.update([stroke(3).into()], [], None, at(100));
    sheet.update([], stroke(4), None, at(1_000));
    sheet.update([stroke(4).into()], [], None, at(1_100));
    assert_eq!(
        sheet.strokes.len(),
        2,
        "An arrow drawn while the circle fades keeps it"
    );
    sheet.update([], stroke(5), None, at(1_150));
    assert!(sheet.update([stroke(5).into()], stroke(2), None, at(1_200)));
    assert!(
        whole(&sheet, at(9_000)),
        "A stroke cut for length while the pen keeps drawing stays whole"
    );
    assert_eq!(sheet.strokes.len(), 3);
    assert!(
        !sheet.update([], stroke(2), None, at(1_300)),
        "A pointer that moved too little to add ink changes nothing"
    );
    sheet.update([stroke(2).into()], [], None, at(1_400));
    assert_eq!(sheet.next_frame(at(1_400)), Redraw::After(HOLD));
    assert!(sheet.update([], stroke(3), None, at(2_800)));
    assert_eq!(
        sheet.strokes,
        Vec::<Arc<[Point]>>::new(),
        "Ink once everything has faded starts a clean sheet"
    );
    assert!(!Sheet::new(Part::WHOLE).update([], [], None, at(0)));
}

fn mark(number: usize, strokes: Range<usize>) -> Mark {
    Mark {
        number,
        shape: Shape::Tap,
        strokes,
        span: Span::at(Moment::START),
        bounds: Rect {
            left: 0.5,
            top: 0.5,
            right: 0.5,
            bottom: 0.5,
        },
    }
}

fn marks(count: usize) -> Arc<[Mark]> {
    (1..=count).map(|number| mark(number, 0..0)).collect()
}

fn grouped(ends: &[usize]) -> Arc<[Mark]> {
    let starts = std::iter::once(0).chain(ends.iter().copied());
    (1..)
        .zip(starts.zip(ends))
        .map(|(number, (start, &end))| mark(number, start..end))
        .collect()
}

fn shown(sheet: &Sheet) -> Vec<usize> {
    sheet.shown().map(|mark| mark.number).collect()
}

#[test]
fn numbers_show_beside_their_marks_until_the_ink_fades() {
    let at = clock();
    let mut sheet = Sheet::new(Part::WHOLE);
    sheet.update([], stroke(3), None, at(0));
    assert!(sheet.update([stroke(3).into()], [], Some(marks(1)), at(100)));
    assert_eq!(shown(&sheet), [1]);
    assert!(sheet.update([], [], Some(marks(2)), at(700)));
    assert_eq!(shown(&sheet), [1, 2], "A pin keeps the ink before it");
    assert_eq!(
        sheet.next_frame(at(700)),
        Redraw::After(HOLD),
        "A pin holds the ink again"
    );
    assert!(sheet.update([], [], Some(marks(3)), at(2_100)));
    assert_eq!(
        shown(&sheet),
        [3],
        "Once everything has faded, a pin starts clean, numbered on from the last mark"
    );
    assert_eq!(sheet.strokes, Vec::<Arc<[Point]>>::new());
    assert!(whole(&sheet, at(2_100)));
    sheet.update([], stroke(4), None, at(4_000));
    sheet.update([stroke(4).into()], [], Some(marks(4)), at(4_100));
    assert_eq!(
        shown(&sheet),
        [4],
        "Once everything has faded, a stroke starts clean too"
    );
}

#[test]
fn a_stroke_that_joins_a_faded_mark_shows_its_number() {
    let at = clock();
    let mut sheet = Sheet::new(Part::WHOLE);
    sheet.update([], stroke(3), None, at(0));
    sheet.update([stroke(3).into()], [], Some(grouped(&[1])), at(100));
    sheet.update([], stroke(4), None, at(1_450));
    assert_eq!(
        sheet.strokes,
        Vec::<Arc<[Point]>>::new(),
        "Ink that arrives after the circle has faded starts a clean sheet"
    );
    sheet.update([stroke(4).into()], [], Some(grouped(&[2])), at(1_700));
    assert_eq!(
        shown(&sheet),
        [1],
        "A stroke the bundle joins to a faded mark shows that mark's number"
    );
    sheet.update([], stroke(5), None, at(4_000));
    sheet.update([stroke(5).into()], [], Some(grouped(&[2, 3])), at(4_100));
    assert_eq!(
        shown(&sheet),
        [2],
        "A stroke after everything has faded again starts a mark of its own"
    );
}

#[test]
fn a_drag_chooses_the_area_it_covers_and_a_click_chooses_nothing() {
    let size = gpui::size(px(1_000.0), px(500.0));
    let at = |x, y| point(px(x), px(y));
    let part = |left, top, right, bottom| Part {
        left,
        top,
        right,
        bottom,
    };
    assert_eq!(
        dragged(at(300.0, 200.0), at(100.0, 100.0), size),
        Some(part(0.1, 0.2, 0.3, 0.4)),
        "Dragged up and to the left"
    );
    assert_eq!(
        dragged(at(900.0, 400.0), at(1_200.0, 600.0), size),
        Some(part(0.9, 0.8, 1.0, 1.0)),
        "Past the display's edge, the area stops at it"
    );
    assert_eq!(dragged(at(100.0, 100.0), at(110.0, 300.0), size), None);
    assert_eq!(dragged(at(100.0, 100.0), at(100.0, 100.0), size), None);
}

#[test]
fn a_click_instead_of_a_drag_stops_waiting_for_an_area() {
    let size = gpui::size(px(1_000.0), px(500.0));
    let at = |x, y| point(px(x), px(y));
    let (chosen, choice) = async_channel::bounded(1);
    assert!(answer(&chosen, at(100.0, 100.0), at(300.0, 200.0), size));
    assert!(choice.try_recv().is_ok(), "A drag sent no area");
    assert!(!answer(&chosen, at(100.0, 100.0), at(100.0, 100.0), size));
    assert!(
        choice.is_closed(),
        "A click left the Show waiting for an area"
    );
}
