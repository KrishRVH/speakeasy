use super::*;
use crate::movie::tests::frame;

#[test]
fn resized_frames_clear_old_pixels_and_keep_aspect_ratio() -> anyhow::Result<()> {
    let mut canvas = Canvas::new([4, 4])?;
    canvas.paint(&frame(4, 4, 200))?;
    canvas.paint(&frame(4, 2, 100))?;
    let pixels = canvas.held();
    assert_eq!(&pixels[..16], &[0; 16]);
    assert_eq!(&pixels[16..48], &[100, 100, 100, 255].repeat(8));
    assert_eq!(&pixels[48..], &[0; 16]);
    canvas.paint(&frame(2, 4, 75))?;
    let pixels = canvas.held();
    for row in pixels.as_chunks::<16>().0 {
        assert_eq!(&row[..4], &[0; 4]);
        assert_eq!(&row[4..12], &[75, 75, 75, 255].repeat(2));
        assert_eq!(&row[12..], &[0; 4]);
    }
    let mut invalid = frame(4, 4, 0);
    invalid.bgra.pop();
    assert!(canvas.paint(&invalid).is_err());
    assert!(Canvas::new([u32::MAX, 1]).is_err());
    let mut grid = frame(8, 8, 0);
    for (index, pixel) in grid.bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let shade = if index % 2 == 0 { 0 } else { 255 };
        *pixel = [shade, shade, shade, 255];
    }
    canvas.paint(&grid)?;
    assert!(
        canvas
            .held()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| *pixel == [128, 128, 128, 255]),
        "Downsampling aliases thin lines instead of blending them"
    );
    Ok(())
}

#[test]
fn content_rectangles_equal_painted_pixels_after_odd_and_tiny_resizes() -> anyhow::Result<()> {
    let mut canvas = Canvas::new([8, 6])?;
    for [width, height] in [[8, 6], [3, 5], [7, 3], [1, 1], [13, 17], [2560, 1]] {
        let mut frame = frame(width, height, 150);
        frame.part.right = 0.5;
        canvas.paint(&frame)?;
        assert_eq!(canvas.part(), frame.part);
        let [columns, _] = canvas.size();
        let columns = usize::try_from(columns)?;
        let [left, top, width, height] = canvas.content();
        assert!(width > 0 && height > 0);
        for (index, pixel) in canvas.held().as_chunks::<4>().0.iter().enumerate() {
            let x = u32::try_from(index % columns)?;
            let y = u32::try_from(index / columns)?;
            let inside = (left..left + width).contains(&x) && (top..top + height).contains(&y);
            assert_eq!(*pixel, if inside { [150, 150, 150, 255] } else { [0; 4] });
        }
    }
    Ok(())
}
