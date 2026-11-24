use super::*;

#[test]
#[ignore = "Opt in: moves an owned window and reads only its geometry; no screen capture or input"]
fn target_geometry_tracks_an_owned_window_without_sampling_input() -> anyhow::Result<()> {
    per_monitor_aware()?;

    let display =
        describe(unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) })?;

    let _class = Class::register(SUBJECT_CLASS, Some(DefWindowProcW), brush(GRAY))?;
    let [left, top, right, bottom] = Layout::centered_on(display.native).pattern;
    let subject = Popup::create(SUBJECT_CLASS, TOPMOST, [left, top, right, bottom])?;
    subject.show();
    dispatch_messages_for(COMPOSED);
    let original = window_part(display.native, subject.window())
        .map_err(|gone| anyhow!(gone))?
        .context("The owned target has no visible frame")?;
    let target = Target {
        display,
        region: Region::Window(subject.window(), original),
    };
    let area = Target::area(display, original);
    let moved = RECT {
        left: left.saturating_add(32),
        top: top.saturating_add(24),
        right,
        bottom,
    };

    let positioned = unsafe {
        SetWindowPos(
            subject.0,
            null_mut(),
            moved.left,
            moved.top,
            moved.right.saturating_sub(moved.left),
            moved.bottom.saturating_sub(moved.top),
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    ensure!(positioned != FALSE, "Can't move the owned test window");
    dispatch_messages_for(COMPOSED);
    let expected = display
        .native
        .bounds()
        .part_covered_by(Bounds::from(moved))
        .context("Moved test window is outside its display")?;
    assert_eq!(pointer_part(target), Some(expected));
    assert_ne!(expected, original);
    assert_eq!(pointer_part(area), Some(original));
    let fractional = Part::between([0.123_456, 0.234_567], [0.567_891, 0.678_912]);
    let normalized = pointer_part(Target::area(display, fractional)).unwrap();
    let [width, height] = display.native.extent();
    let size = [u32::try_from(width)?, u32::try_from(height)?];
    assert_ne!(normalized, fractional);
    assert_eq!(normalized, part_of_box(pixel_box(fractional, size), size));

    unsafe { ShowWindow(subject.0, SW_HIDE) };
    assert!(pointer_part(target).is_none());
    assert_eq!(pointer_part(area), Some(original));
    Ok(())
}
