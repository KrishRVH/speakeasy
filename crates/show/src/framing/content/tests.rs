use super::*;

const SCREEN: Size = Size {
    width: 1_440.0,
    height: 900.0,
};

fn page(text: &[[u32; 4]], outlines: &[[u32; 4]]) -> Image {
    let (width, height) = (1_440_u32, 900_u32);
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let striped = text.iter().any(|&[left, top, right, bottom]| {
                (left..right).contains(&x) && (top..bottom).contains(&y) && x % 4 < 2
            });
            let bordered = outlines.iter().any(|&[left, top, right, bottom]| {
                let inside = (left..right).contains(&x) && (top..bottom).contains(&y);
                let (near_left, near_top) = (left.saturating_add(2), top.saturating_add(2));
                let (near_right, near_bottom) = (right.saturating_sub(2), bottom.saturating_sub(2));
                inside && (x < near_left || x >= near_right || y < near_top || y >= near_bottom)
            });
            let inked = striped || bordered;
            let level = if inked { 30 } else { 250 };
            rgba.extend([level, level, level, 255]);
        }
    }
    Image::from_rgba(width, height, rgba).unwrap()
}

fn snapped(text: &[[u32; 4]], crop: [f32; 4]) -> Rect {
    framed(text, &[], crop)
}

fn framed(text: &[[u32; 4]], outlines: &[[u32; 4]], crop: [f32; 4]) -> Rect {
    Content::of(&page(text, outlines), SCREEN).snap(
        Rect::from(crop),
        Screen::of(SCREEN),
        Rect::UNIT,
    )
}

#[test]
fn an_edge_through_a_line_of_text_moves_out_past_it() {
    let label = [400, 300, 700, 312];
    let crop = snapped(&[label], [0.4, 0.2, 0.7, 0.6]);
    assert!(
        crop.left * 1_440.0 <= 400.0 && crop.left * 1_440.0 >= 380.0,
        "{crop:?}"
    );
    assert_eq!(crop.right, 0.7);
    assert_eq!((crop.top, crop.bottom), (0.2, 0.6));
}

#[test]
fn an_edge_through_a_row_moves_below_it_and_an_edge_in_white_space_stays() {
    let row = [600, 500, 900, 530];
    let crop = snapped(&[row], [0.35, 0.3, 0.7, 0.57]);
    assert!(
        crop.bottom * 900.0 >= 530.0 && crop.bottom * 900.0 <= 540.0,
        "{crop:?}"
    );
    assert_eq!((crop.left, crop.right, crop.top), (0.35, 0.7, 0.3));
}

#[test]
fn text_running_past_the_reach_keeps_the_edge_and_a_column_at_the_screen_edge_joins_the_crop() {
    let crop = [0.5, 0.2, 0.7, 0.6];
    let long = [100, 300, 1_300, 312];
    assert_eq!(snapped(&[long], crop), Rect::from(crop));
    let column = [0, 100, 200, 800];
    assert_eq!(snapped(&[column], [0.05, 0.2, 0.7, 0.6]).left, 0.0);
}

#[test]
fn an_edge_through_a_button_moves_past_its_border() {
    let button = [700, 480, 800, 520];
    let crop = framed(&[], &[button], [0.35, 0.3, 0.7, 0.555]);
    assert!(
        crop.bottom * 900.0 >= 520.0 && crop.bottom * 900.0 <= 540.0,
        "{crop:?}"
    );
    let crop = framed(&[], &[button], [0.35, 0.555, 0.7, 0.8]);
    assert!(
        crop.top * 900.0 <= 480.0 && crop.top * 900.0 >= 460.0,
        "{crop:?}"
    );
}

#[test]
fn a_line_running_to_the_screen_edge_takes_the_crop_edge_with_it() {
    let crop = snapped(&[[6, 300, 700, 312]], [0.1, 0.2, 0.7, 0.6]);
    assert_eq!(crop.left, 0.0, "{crop:?}");
    let crop = snapped(&[[800, 300, 1_436, 312]], [0.3, 0.2, 0.9, 0.6]);
    assert_eq!(crop.right, 1.0, "{crop:?}");
}

#[test]
fn snapping_stops_at_the_window_it_frames() {
    let label = [400, 300, 700, 312];
    let crop = Rect::from([0.4, 0.2, 0.7, 0.6]);
    let window = Rect::from([0.35, 0.1, 0.8, 0.7]);
    let snapped = Content::of(&page(&[label], &[]), SCREEN).snap(crop, Screen::of(SCREEN), window);
    assert_eq!(snapped, Rect { left: 0.35, ..crop });
}
