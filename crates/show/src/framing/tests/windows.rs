use super::*;
use crate::{InkPoint, Layer, Stroke, Window};

fn window(id: u32, app: u32, layer: Layer, bounds: [f32; 4]) -> Window {
    Window {
        id,
        app,
        layer,
        bounds: Rect::from(bounds),
    }
}

fn arranged(at: u64, windows: Vec<Window>) -> Layout {
    Layout {
        at: Moment::from_millis(at),
        windows,
    }
}

fn planned(clicks: &[(u64, Point)], at: u64, changes: &[Rect], layouts: &[Layout]) -> Option<Plan> {
    let keyframes = [keyframe(1, at)];
    let changes = [changes.to_vec()];
    plan(&clicked(SCREEN, clicks), &keyframes, &[], &changes, layouts)
        .into_iter()
        .next()
        .flatten()
}

#[test]
fn a_click_frames_its_whole_window_so_a_label_across_the_row_stays_in_frame() {
    let settings = window(1, 0, Layer::Normal, [0.1, 0.1, 0.89, 0.9]);
    let behind = window(2, 1, Layer::Normal, [0.0, 0.0, 0.6, 0.6]);
    let toggle = Point::new(0.86, 0.3);
    let label = Point::new(0.2, 0.3);
    let elsewhere = Rect::from([0.88, 0.25, 0.99, 0.35]);
    let layouts = [arranged(0, vec![settings, behind])];
    let plan = planned(&[(5_000, toggle)], 5_000, &[elsewhere], &layouts).unwrap();
    assert!(plan.crop.encloses(settings.bounds), "{plan:?}");
    assert!(plan.crop.contains(label) && plan.crop.contains(toggle));
    assert!(plan.crop.right < elsewhere.right, "{plan:?}");
    assert_eq!(plan.within, plan.crop);
}

#[test]
fn a_menu_belongs_to_its_apps_window_and_a_moved_window_is_framed_where_it_ended() {
    let editor = window(1, 0, Layer::Normal, [0.1, 0.2, 0.5, 0.6]);
    let menu = window(2, 0, Layer::Menu, [0.12, 0.03, 0.3, 0.4]);
    let bar = window(3, 9, Layer::Bar, [0.0, 0.0, 1.0, 0.03]);
    let item = Point::new(0.2, 0.3);
    let layouts = [arranged(0, vec![menu, editor, bar])];
    let plan = planned(&[(1_000, item)], 1_000, &[], &layouts).unwrap();
    assert!(
        plan.crop.encloses(editor.bounds) && plan.crop.encloses(menu.bounds),
        "{plan:?}"
    );

    let moved = Window {
        bounds: Rect::from([0.45, 0.35, 0.85, 0.75]),
        ..editor
    };
    let layouts = [arranged(0, vec![editor]), arranged(2_000, vec![moved])];
    let plan = planned(&[(500, Point::new(0.3, 0.4))], 3_000, &[], &layouts).unwrap();
    assert!(plan.crop.encloses(moved.bounds), "{plan:?}");
}

#[test]
fn input_spread_across_many_windows_frames_the_input_instead() {
    let layouts = [arranged(
        0,
        vec![
            window(1, 0, Layer::Normal, [0.0, 0.0, 0.33, 0.5]),
            window(2, 1, Layer::Normal, [0.33, 0.0, 0.66, 0.5]),
            window(3, 2, Layer::Normal, [0.66, 0.0, 1.0, 0.5]),
        ],
    )];
    let clicks = [
        (1_000, Point::new(0.2, 0.1)),
        (1_200, Point::new(0.4, 0.1)),
        (1_400, Point::new(0.7, 0.1)),
    ];
    let plan = planned(&clicks, 1_200, &[], &layouts).unwrap();
    assert_eq!(plan.within, Rect::UNIT);
}

#[test]
fn without_input_the_front_window_is_framed_and_a_nearly_full_window_keeps_the_focus() {
    let front = window(1, 0, Layer::Normal, [0.2, 0.1, 0.8, 0.8]);
    let behind = window(2, 1, Layer::Normal, [0.0, 0.0, 0.5, 0.5]);
    let layouts = [arranged(0, vec![front, behind])];
    let plan = planned(&[], 1_000, &[], &layouts).unwrap();
    assert!(plan.crop.encloses(front.bounds), "{plan:?}");
    assert!(!plan.crop.encloses(behind.bounds), "{plan:?}");

    let full = window(1, 0, Layer::Normal, [0.0, 0.04, 1.0, 1.0]);
    let click = Point::new(0.85, 0.1);
    let plan = planned(&[(1_000, click)], 1_000, &[], &[arranged(0, vec![full])]).unwrap();
    assert!(
        plan.crop.contains(click) && plan.crop.area() < 0.5,
        "{plan:?}"
    );
    assert!(
        plan.within.encloses(plan.crop) && plan.within.top > 0.0,
        "{plan:?}"
    );
}

#[test]
fn a_dialog_the_click_opened_is_framed_with_its_window() {
    let editor = window(1, 0, Layer::Normal, [0.1, 0.1, 0.45, 0.5]);
    let dialog = window(2, 0, Layer::Normal, [0.35, 0.35, 0.65, 0.6]);
    let other = window(3, 1, Layer::Normal, [0.5, 0.0, 1.0, 0.7]);
    let sibling = window(4, 0, Layer::Normal, [0.7, 0.75, 0.95, 0.95]);
    let delete = Point::new(0.2, 0.4);
    let layouts = [
        arranged(0, vec![sibling, editor, other]),
        arranged(1_100, vec![sibling, dialog, editor, other]),
    ];
    let plan = planned(&[(1_000, delete)], 1_500, &[], &layouts).unwrap();
    assert!(
        plan.crop.encloses(editor.bounds) && plan.crop.encloses(dialog.bounds),
        "{plan:?}"
    );
    assert!(!plan.crop.encloses(other.bounds), "{plan:?}");
    assert!(!plan.crop.contains(Point::new(0.9, 0.9)), "{plan:?}");
}

#[test]
fn a_floating_window_neither_leads_nor_hides_the_windows_beneath_it() {
    let call = window(1, 0, Layer::Floating, [0.78, 0.05, 0.98, 0.25]);
    let overlay = window(2, 1, Layer::Menu, [0.0, 0.0, 1.0, 1.0]);
    let editor = window(3, 2, Layer::Normal, [0.05, 0.1, 0.55, 0.7]);
    let layouts = [arranged(0, vec![call, overlay, editor])];
    let plan = planned(&[], 1_000, &[], &layouts).unwrap();
    assert!(plan.crop.encloses(editor.bounds), "{plan:?}");
    assert!(!plan.crop.contains(Point::new(0.9, 0.15)), "{plan:?}");

    let click = Point::new(0.3, 0.3);
    let plan = planned(&[(1_000, click)], 1_000, &[], &layouts).unwrap();
    assert!(plan.crop.encloses(editor.bounds), "{plan:?}");
    assert_eq!(plan.within, plan.crop);
}

#[test]
fn a_long_stroke_counts_as_one_gesture_against_clicks() {
    let form = window(1, 0, Layer::Normal, [0.05, 0.1, 0.45, 0.8]);
    let page = window(2, 1, Layer::Normal, [0.55, 0.1, 0.95, 0.8]);
    let circled: Vec<InkPoint> = (0..60_u16)
        .map(|step| {
            let turn = f32::from(step) / 60.0 * std::f32::consts::TAU;
            InkPoint::from((
                0.75 + 0.05 * turn.cos(),
                0.4 + 0.05 * turn.sin(),
                Moment::from_millis(2_000 + u64::from(step) * 10),
            ))
        })
        .collect();
    let clicks = [
        (2_800, Point::new(0.1, 0.3)),
        (2_900, Point::new(0.1, 0.4)),
        (3_000, Point::new(0.1, 0.5)),
    ];
    let recording = Recording {
        strokes: vec![Stroke::new(circled).unwrap()],
        ..clicked(SCREEN, &clicks)
    };
    let keyframes = [keyframe(1, 3_000)];
    let layouts = [arranged(0, vec![form, page])];
    let plan = plan(&recording, &keyframes, &[], &[Vec::new()], &layouts)[0].unwrap();
    assert!(
        plan.crop.encloses(form.bounds) && plan.crop.encloses(page.bounds),
        "{plan:?}"
    );
}

#[test]
fn without_input_the_window_that_changed_joins_the_front_window() {
    let editor = window(1, 0, Layer::Normal, [0.0, 0.1, 0.45, 0.9]);
    let preview = window(2, 1, Layer::Normal, [0.55, 0.1, 1.0, 0.9]);
    let reloaded = Rect::from([0.6, 0.2, 0.9, 0.5]);
    let layouts = [arranged(0, vec![editor, preview])];
    let plan = planned(&[], 1_000, &[reloaded], &layouts).unwrap();
    assert!(plan.crop.encloses(editor.bounds), "{plan:?}");
    assert!(plan.crop.encloses(reloaded), "{plan:?}");

    let typed = Rect::from([0.1, 0.3, 0.3, 0.4]);
    let plan = planned(&[], 1_000, &[typed], &layouts).unwrap();
    assert!(!plan.crop.encloses(reloaded), "{plan:?}");

    let call = window(3, 2, Layer::Floating, [0.6, 0.2, 0.9, 0.5]);
    let layouts = [arranged(0, vec![call, editor, preview])];
    let plan = planned(&[], 1_000, &[reloaded], &layouts).unwrap();
    assert!(!plan.crop.encloses(reloaded), "{plan:?}");
}

#[test]
fn a_large_window_short_of_the_whole_screen_keeps_a_far_label_in_frame() {
    let settings = window(1, 0, Layer::Normal, [0.08, 0.06, 0.92, 0.94]);
    let label = Point::new(0.12, 0.5);
    let toggle = Point::new(0.9, 0.5);
    let layouts = [arranged(0, vec![settings])];
    let plan = planned(&[(1_000, toggle)], 1_000, &[], &layouts).unwrap();
    assert!(
        plan.crop.contains(label) && plan.crop.contains(toggle),
        "{plan:?}"
    );
}
