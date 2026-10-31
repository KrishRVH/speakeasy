use super::*;

#[test]
#[ignore = "Opt in: moves an owned window and reads only its geometry; no screen capture or input"]
fn target_geometry_tracks_an_owned_window_without_sampling_input() -> anyhow::Result<()> {
    per_monitor_aware()?;
    // SAFETY: a fixed desktop point selects a display without reading the user's pointer.
    let display =
        describe(unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) })?;
    // SAFETY: the registered class owns this brush until its own windows are destroyed.
    let gray = unsafe { CreateSolidBrush(0x0060_6060) };
    let _class = Class::register(SUBJECT_CLASS, Some(DefWindowProcW), gray)?;
    let [left, top, right, bottom] = Layout::centered_on(display.native).pattern;
    let window = Popup::create(
        SUBJECT_CLASS,
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        [left, top, right, bottom],
    )?;
    window.show();
    dispatch_messages_for(COMPOSED);
    let original = window_part(display.native, Window(window.0 as isize))
        .map_err(|gone| anyhow!(gone))?
        .context("The owned target has no visible frame")?;
    let target = Target {
        display,
        region: Region::Window(Window(window.0 as isize), original),
    };
    let area = Target::area(display, original);
    let moved = RECT {
        left: left.saturating_add(32),
        top: top.saturating_add(24),
        right,
        bottom,
    };
    // SAFETY: only this test's live window moves; flags preserve activation and stacking.
    let positioned = unsafe {
        SetWindowPos(
            window.0,
            null_mut(),
            moved.left,
            moved.top,
            moved.right.saturating_sub(moved.left),
            moved.bottom.saturating_sub(moved.top),
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
    ensure!(positioned != FALSE, "Cannot move the owned test window");
    dispatch_messages_for(COMPOSED);
    let expected = display
        .native
        .bounds()
        .part_covered_by(Bounds::from(moved))
        .context("Moved test window is outside its display")?;
    let mut elapsed = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        let start = Instant::now();
        assert_eq!(pointer_part(target), Some(expected));
        elapsed.push(start.elapsed());
    }
    assert_ne!(expected, original);
    assert_eq!(pointer_part(area), Some(original));
    let fractional = Part::between([0.123_456, 0.234_567], [0.567_891, 0.678_912]);
    let normalized = pointer_part(Target::area(display, fractional)).unwrap();
    let [width, height] = display.native.extent();
    let size = [u32::try_from(width)?, u32::try_from(height)?];
    assert_ne!(normalized, fractional);
    assert_eq!(normalized, part_of_box(pixel_box(fractional, size), size));
    let mut stale = area;
    stale.display.native.edges[2] = stale.display.native.edges[2].saturating_add(1);
    assert!(pointer_part(stale).is_none());
    elapsed.sort_unstable();
    println!(
        "Owned target geometry: median {:?}, p95 {:?} across 1000 reads",
        elapsed[500], elapsed[950]
    );
    // SAFETY: hiding this test's window neither changes focus nor injects input.
    unsafe { ShowWindow(window.0, SW_HIDE) };
    assert!(pointer_part(target).is_none());
    assert_eq!(pointer_part(area), Some(original));
    Ok(())
}
