//! Checks on this machine's GPU and display. The capture check shows Speakeasy's own windows
//! for a few seconds and tallies captured pixels only after proving, before and after each
//! capture, that no other window covers them; it prints nothing but counts and timings.

use std::ptr::{null, null_mut};

use anyhow::bail;
use windows::Win32::Graphics::Direct3D11::D3D11_SUBRESOURCE_DATA;
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, TRUE, WPARAM},
        Graphics::{
            Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Gdi::{
                BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, CreateSolidBrush, DIB_RGB_COLORS,
                EndPaint, EnumDisplayMonitors, HBRUSH, HDC, MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT,
                SetDIBitsToDevice,
            },
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{
                AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                GetDpiAwarenessContextForProcess, SetProcessDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GA_ROOT,
                GWL_EXSTYLE, GetAncestor, GetClassNameW, GetPropW, GetWindowLongPtrW,
                GetWindowRect, IsWindowVisible, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
                SWP_NOZORDER, SetWindowDisplayAffinity, TranslateMessage, UnregisterClassW,
                WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WDA_NONE, WM_DPICHANGED, WM_PAINT, WNDCLASSW,
                WNDPROC, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
                WS_EX_TRANSPARENT, WS_POPUP, WindowFromPoint,
            },
        },
    },
    core::{BOOL, PCWSTR},
    w,
};

use super::{
    exclusion::{ALWAYS_EXCLUDED, affinity, owned, top_level_windows},
    readback::{BYTES_PER_PIXEL, created, texture_description},
    *,
};
use crate::{ScreenCapture, windows::native_window};

const PATTERN_WIDTH: i32 = 640;
const PATTERN_HEIGHT: i32 = 400;
/// The backdrop's border around the pattern; even, so halved frames keep its edges.
const MARGIN: i32 = 32;
/// The overlay's place on the pattern: left, top, right, bottom.
const OVERLAY: [i32; 4] = [200, 120, 440, 280];
const BLACK: [u8; 3] = [0; 3];
const MAGENTA: [u8; 3] = [255, 0, 255];
/// How far a captured channel may stray from the painted one.
const TOLERANCE: u8 = 6;
/// The longest the windows stay on screen, whatever happens to the test.
const ON_SCREEN: Duration = Duration::from_secs(6);
/// Lets DWM compose a change before it is captured.
const COMPOSED: Duration = Duration::from_millis(300);
const PATIENCE: Duration = Duration::from_secs(3);
const INTERVAL: Duration = Duration::from_millis(100);
/// How long frames are watched after a window opens mid-capture.
const WATCH: Duration = Duration::from_secs(1);
#[expect(clippy::cast_possible_truncation, reason = "The header is 40 bytes")]
const HEADER_SIZE: u32 = size_of::<BITMAPINFOHEADER>() as u32;
/// The shell's pen handwriting canvas spans the primary display above every app, but stays
/// transparent unless a pen writes into a text box; ink there would fail the pixel check.
const HANDWRITING_CANVAS: &str = "ShellHandwritingCanvas";
const BACKDROP_CLASS: PCWSTR = w!("SpeakeasyCaptureBackdrop");
const PATTERN_CLASS: PCWSTR = w!("SpeakeasyCapturePattern");
const OVERLAY_CLASS: PCWSTR = w!("SpeakeasyCaptureOverlay");
const RESCALING_CLASS: PCWSTR = w!("SpeakeasyRescalingOverlay");

/// The pattern's RGB color at `[x, y]`: four quadrants above a checkerboard band. Every
/// edge falls on an even pixel, so halving reproduces it exactly.
fn pattern(x: i32, y: i32) -> [u8; 3] {
    if y >= 320 {
        return if ((x >> 3) ^ (y >> 3)) & 1 == 0 {
            [255; 3]
        } else {
            BLACK
        };
    }
    match (x < PATTERN_WIDTH / 2, y < 160) {
        (true, true) => [220, 40, 40],
        (false, true) => [40, 180, 60],
        (true, false) => [40, 80, 220],
        (false, false) => [230, 200, 40],
    }
}

/// Which of the scene's windows a capture includes.
#[derive(Clone, Copy)]
struct Shown {
    pattern: bool,
    overlay: bool,
}

/// What capture shows at `[x, y]` of the backdrop: black, as Windows shows the backdrop to
/// capture, beneath whichever of the pattern and the overlay it includes.
fn captured_color(x: i32, y: i32, shown: Shown) -> [u8; 3] {
    let [x, y] = [x.saturating_sub(MARGIN), y.saturating_sub(MARGIN)];
    let [left, top, right, bottom] = OVERLAY;
    if shown.overlay && (left..right).contains(&x) && (top..bottom).contains(&y) {
        MAGENTA
    } else if shown.pattern && (0..PATTERN_WIDTH).contains(&x) && (0..PATTERN_HEIGHT).contains(&y) {
        pattern(x, y)
    } else {
        BLACK
    }
}

#[test]
#[ignore = "Shows owned windows briefly; tallies captured pixels only where proven theirs"]
fn capture_leaves_out_every_speakeasy_window_until_it_stops() -> anyhow::Result<()> {
    per_monitor_aware()?;
    let display = display_under_pointer()?;
    let mut scene = Scene::open(display.native)?;
    thread::sleep(COMPOSED);
    scene.check_overlay()?;

    // Shown to capture, the overlay proves it is in place, and the backdrop proves Windows
    // blacks it out, so a capture leaving out the other windows shows nothing behind them.
    set_affinity(scene.overlay, WDA_NONE)?;
    thread::sleep(COMPOSED);
    let before = scene.unfiltered(whole(display), u32::MAX)?;
    let shown = |pattern, overlay| move |x, y| captured_color(x, y, Shown { pattern, overlay });
    assert_eq!(
        before.mismatches(shown(false, true)),
        0,
        "Unfiltered, before"
    );

    let (during, opened) = scene.record_opening_pattern(display)?;
    let pattern = scene.pattern.context("Missing pattern window")?;
    assert!(
        during.iter().any(|crop| crop.captured > opened),
        "No frame arrived after the pattern opened"
    );
    for crop in &during {
        assert_eq!(
            crop.mismatches(shown(false, false)),
            0,
            "During the capture"
        );
    }
    assert_eq!(
        affinity(pattern),
        Some(WDA_NONE),
        "Restored after the capture"
    );
    assert_eq!(
        affinity(scene.overlay),
        Some(WDA_EXCLUDEFROMCAPTURE),
        "configure_overlay's exclusion must outlast the capture"
    );

    thread::sleep(COMPOSED);
    let after = scene.unfiltered(whole(display), u32::MAX)?;
    assert_eq!(after.mismatches(shown(true, false)), 0, "Unfiltered, after");
    let [width, height] = display.native.extent();
    let half = width.max(height).cast_unsigned() / 2;
    let halved = scene.unfiltered(whole(display), half)?;
    assert_eq!(halved.scale, 2);
    assert_eq!(halved.mismatches(shown(true, false)), 0, "Halved, after");

    let windows = top_level_windows()?
        .into_iter()
        .filter(|&window| owned(window));
    let own = windows.count();
    let mut exclusions = Exclusions::default();
    exclusions.refresh()?;
    let started = Instant::now();
    for _ in 0..100 {
        exclusions.refresh()?;
    }
    let each = started.elapsed().as_secs_f64() * 1e4;
    println!("Re-checking {own} own windows: {each:.1} µs");
    Ok(())
}

#[test]
#[ignore = "Shows owned windows briefly; tallies captured pixels only where proven theirs"]
fn a_window_or_area_capture_reads_back_only_its_part() -> anyhow::Result<()> {
    per_monitor_aware()?;
    let display = display_under_pointer()?;
    let scene = Scene::open(display.native)?;
    thread::sleep(COMPOSED);
    set_affinity(scene.overlay, WDA_NONE)?;
    thread::sleep(COMPOSED);
    let area = scene.area()?;
    let RECT {
        left,
        top,
        right,
        bottom,
    } = area;
    let part = display
        .native
        .area()
        .part_covered_by(Bounds {
            origin: [left, top].map(f64::from),
            size: [right - left, bottom - top].map(f64::from),
        })
        .context("The scene lies off its display")?;
    let shown = |x, y| {
        captured_color(
            x,
            y,
            Shown {
                pattern: false,
                overlay: true,
            },
        )
    };
    let window = super::Window(scene.backdrop as isize);
    for region in [Region::Window(window, part), Region::Area(part)] {
        let target = Target { display, region };
        let crop = scene.unfiltered(target, u32::MAX)?;
        assert_eq!(crop.mismatches(shown), 0, "{region:?}");
        assert_eq!(crop.part, part, "{region:?}");
        assert_eq!(
            crop.width, crop.frame_width,
            "{region:?} read back only its part"
        );
    }
    Ok(())
}

#[test]
#[ignore = "Uses the GPU on synthetic frames; captures nothing"]
fn gpu_halving_reads_back_exact_pixels() -> anyhow::Result<()> {
    // SAFETY: takes a point by value and returns the primary monitor.
    let primary = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let (device, context) = device_for(HMONITOR(primary))?;
    for source in [[1920, 1080], [3840, 2160]] {
        let frame = synthetic_frame(&device, source)?;
        // The whole frame, then a quarter of it starting on a block boundary.
        let [width, height] = source;
        for (origin, size) in [
            ([0, 0], source),
            ([width / 4, height / 4], [width / 2, height / 2]),
        ] {
            for longest_edge in [u32::MAX, 2560, 960] {
                let readback = Readback::new(&device, size, longest_edge)?;
                let bgra = readback.read(&context, &frame, origin)?;
                let [width, height] = readback.size;
                let [left, top] = origin;
                let expected: Vec<u8> = (0..height)
                    .flat_map(|y| (0..width).map(move |x| [x, y]))
                    .flat_map(|[x, y]| {
                        block_color(left + (x << readback.level), top + (y << readback.level))
                    })
                    .collect();
                assert!(
                    bgra == expected,
                    "{size:?} at {origin:?} of {source:?} read back at {width}×{height}"
                );
                let [median, slowest] = cost(&readback, &context, &frame, origin)?;
                println!(
                    "{}×{} → {width}×{height}: median {median:.2} ms, slowest {slowest:.2} ms",
                    size[0], size[1],
                );
            }
        }
    }
    Ok(())
}

/// The median and slowest of 30 readbacks of `frame` at `origin`, in milliseconds.
fn cost(
    readback: &Readback,
    context: &ID3D11DeviceContext,
    frame: &ID3D11Texture2D,
    origin: [u32; 2],
) -> windows::core::Result<[f64; 2]> {
    let mut costs = Vec::new();
    for _ in 0..30 {
        let started = Instant::now();
        readback.read(context, frame, origin)?;
        costs.push(started.elapsed());
    }
    costs.sort();
    Ok([costs[15], costs[29]].map(|cost| cost.as_secs_f64() * 1e3))
}

/// Synthetic frames are constant across 4×4 blocks, so the first two halvings are exact.
fn block_color(x: u32, y: u32) -> [u8; 4] {
    let [x, y] = [x >> 2, y >> 2].map(|block| block.to_le_bytes()[0]);
    [x, y, x ^ y, 255]
}

fn synthetic_frame(
    device: &ID3D11Device,
    [width, height]: [u32; 2],
) -> anyhow::Result<ID3D11Texture2D> {
    let bgra: Vec<u8> = (0..height)
        .flat_map(|y| (0..width).map(move |x| block_color(x, y)))
        .flatten()
        .collect();
    let data = D3D11_SUBRESOURCE_DATA {
        pSysMem: bgra.as_ptr().cast(),
        SysMemPitch: width.checked_mul(4).unwrap(),
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    // SAFETY: the pixels match the description and outlive the synchronous call.
    unsafe {
        device.CreateTexture2D(
            &texture_description([width, height], 1),
            Some(&raw const data),
            Some(&raw mut texture),
        )?;
    }
    Ok(created(texture)?)
}

#[test]
#[ignore = "Moves a hidden owned window across display scales; captures nothing"]
fn overlay_covers_its_display_after_crossing_scales() -> anyhow::Result<()> {
    per_monitor_aware()?;
    let displays = displays()?;
    let scale =
        |display: &crate::screen::Display| display.native.extent()[0] as f32 / display.size[0];
    let Some((from, to)) = displays.iter().find_map(|from| {
        displays
            .iter()
            .find(|to| (scale(to) - scale(from)).abs() > 0.01)
            .map(|to| (from, to))
    }) else {
        let scales: Vec<f32> = displays.iter().map(scale).collect();
        println!("Needs two displays of different scale, not {scales:?}; nothing checked");
        return Ok(());
    };
    let _class = Class::register(RESCALING_CLASS, Some(rescale_like_gpui), null_mut())?;
    let [left, top, ..] = from.native.bounds;
    let window = Window::create(
        RESCALING_CLASS,
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        [left, top, 100, 100],
    )?;
    // SAFETY: reads this thread's own live window.
    let dpi = unsafe { GetDpiForWindow(window.0) };
    // SAFETY: this thread owns the live window until `window` drops.
    configure_overlay(&unsafe { native_window(window.0) }?, to.native)?;
    let mut bounds = RECT::default();
    // SAFETY: reads this thread's own live window into a live local.
    let (moved_dpi, measured) = unsafe {
        (
            GetDpiForWindow(window.0),
            GetWindowRect(window.0, &raw mut bounds),
        )
    };
    ok_or_last_error(measured)?;
    assert_ne!(dpi, moved_dpi, "The window must arrive at another scale");
    let RECT {
        left,
        top,
        right,
        bottom,
    } = bounds;
    assert_eq!([left, top, right, bottom], to.native.bounds);
    Ok(())
}

/// Every display on the desktop.
fn displays() -> anyhow::Result<Vec<crate::screen::Display>> {
    unsafe extern "system" fn collect(
        monitor: windows_sys::Win32::Graphics::Gdi::HMONITOR,
        _: HDC,
        _: *mut RECT,
        monitors: LPARAM,
    ) -> BOOL {
        // SAFETY: `displays` passes its live vector for the length of the enumeration.
        unsafe { (*(monitors as *mut Vec<isize>)).push(monitor as isize) };
        TRUE
    }
    let mut monitors: Vec<isize> = Vec::new();
    // SAFETY: the callback only pushes into the vector, which outlives the enumeration.
    ok_or_last_error(unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect),
            (&raw mut monitors) as LPARAM,
        )
    })?;
    monitors
        .into_iter()
        .map(|monitor| describe(monitor as windows_sys::Win32::Graphics::Gdi::HMONITOR))
        .collect()
}

/// Answers a DPI change as GPUI does: by taking the size and place Windows suggests.
unsafe extern "system" fn rescale_like_gpui(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DPICHANGED {
        // SAFETY: for WM_DPICHANGED, lparam points to the suggested RECT for this call.
        let suggested = unsafe { *(lparam as *const RECT) };
        // SAFETY: moves this thread's own window.
        unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                suggested.left,
                suggested.top,
                suggested.right.saturating_sub(suggested.left),
                suggested.bottom.saturating_sub(suggested.top),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        return 0;
    }
    // SAFETY: forwards unchanged arguments for this thread's own window.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Makes this process per-monitor DPI aware, as the manifest makes Speakeasy, or fails:
/// without it, window and capture coordinates disagree and a crop could leave the scene.
fn per_monitor_aware() -> anyhow::Result<()> {
    // SAFETY: both calls take and return context values only.
    let aware = unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        AreDpiAwarenessContextsEqual(
            GetDpiAwarenessContextForProcess(null_mut()),
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        ) != FALSE
    };
    ensure!(aware, "The test process is not per-monitor DPI aware");
    Ok(())
}

fn set_affinity(window: HWND, affinity: u32) -> anyhow::Result<()> {
    // SAFETY: takes a window of this process by value.
    Ok(ok_or_last_error(unsafe {
        SetWindowDisplayAffinity(window, affinity)
    })?)
}

/// All of `display`.
const fn whole(display: crate::screen::Display) -> Target {
    Target {
        display,
        region: Region::Whole,
    }
}

/// The first frame of `target` from a capture that, unlike `ScreenCapture`, leaves no
/// window out.
fn unfiltered_frame(target: Target, longest_edge: u32) -> anyhow::Result<Frame> {
    join_lasting_apartment()?;
    let (_stop, stopping) = async_channel::bounded::<Infallible>(1);
    let started = Instant::now();
    let mut capture = Capture::start(target, INTERVAL, longest_edge, &stopping)?;
    let deadline = started.checked_add(PATIENCE).unwrap();
    while Instant::now() < deadline {
        if capture.poll(started)?
            && let Some(frame) = capture.read_pending()?
        {
            return Ok(frame);
        }
        thread::sleep(POLL);
    }
    bail!("No frame arrived")
}

/// The next event to arrive before `until`.
fn next_event(events: &Receiver<ScreenEvent>, until: Instant) -> Option<ScreenEvent> {
    while Instant::now() < until {
        if let Ok(event) = events.try_recv() {
            return Some(event);
        }
        thread::sleep(Duration::from_millis(5));
    }
    None
}

fn frame_of(event: ScreenEvent) -> anyhow::Result<Frame> {
    match event {
        ScreenEvent::Frame(frame) => Ok(frame),
        ScreenEvent::Failed(message) => bail!(message),
    }
}

/// The pixels of the scene's backdrop from one captured frame, `scale` physical pixels
/// apiece.
struct Crop {
    scale: usize,
    width: usize,
    captured: Instant,
    bgra: Vec<[u8; 4]>,
    /// The part of the display the frame showed, and its width in frame pixels.
    part: Part,
    frame_width: usize,
}

impl Crop {
    /// Keeps only the part of `frame` showing `area`, then drops the frame.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "Offsets within one captured display, bounded by indexing"
    )]
    fn of(frame: Frame, display: Display, area: RECT) -> anyhow::Result<Self> {
        let [display_left, display_top, ..] = display.bounds;
        let extent = display.extent().map(i32::cast_unsigned);
        let [left, top, right, bottom] = pixel_box(frame.part, extent).map(u32::cast_signed);
        let [left, top] = [display_left + left, display_top + top];
        let [width, height] = [right - left + display_left, bottom - top + display_top];
        let scale = width / frame.width.cast_signed();
        ensure!(
            scale > 0 && frame.height.cast_signed() * scale == height,
            "The frame does not match its part of the display"
        );
        let frame_pixels = |physical: i32| (physical / scale).cast_unsigned() as usize;
        let column = frame_pixels(area.left - left);
        let row = frame_pixels(area.top - top);
        let crop_width = frame_pixels(area.right - area.left);
        let crop_height = frame_pixels(area.bottom - area.top);
        let bgra = frame
            .bgra
            .chunks_exact(frame.width as usize * BYTES_PER_PIXEL)
            .skip(row)
            .take(crop_height)
            .flat_map(|line| {
                line[column * BYTES_PER_PIXEL..(column + crop_width) * BYTES_PER_PIXEL]
                    .as_chunks::<BYTES_PER_PIXEL>()
                    .0
                    .iter()
                    .copied()
            })
            .collect();
        Ok(Self {
            scale: scale.cast_unsigned() as usize,
            width: crop_width,
            captured: frame.captured,
            bgra,
            part: frame.part,
            frame_width: frame.width as usize,
        })
    }

    /// How many pixels stray from what `expected` says capture shows at each physical
    /// offset into the crop.
    fn mismatches(&self, expected: impl Fn(i32, i32) -> [u8; 3]) -> usize {
        let positions = || (0..).step_by(self.scale);
        self.bgra
            .chunks_exact(self.width)
            .zip(positions())
            .map(|(line, y)| {
                line.iter()
                    .zip(positions())
                    .filter(|&(&pixel, x)| !near(pixel, expected(x, y)))
                    .count()
            })
            .sum()
    }
}

fn near([blue, green, red, _]: [u8; 4], [r, g, b]: [u8; 3]) -> bool {
    [red.abs_diff(r), green.abs_diff(g), blue.abs_diff(b)]
        .into_iter()
        .all(|difference| difference <= TOLERANCE)
}

/// Speakeasy's own windows: a backdrop that Windows shows to capture as black, a magenta
/// overlay configured as the Show's, and later a test pattern between them that no
/// exclusion call ever names. A painter thread owns them and destroys them when the scene
/// drops, or after `ON_SCREEN` whatever the test does.
struct Scene {
    backdrop: HWND,
    overlay: HWND,
    overlay_bounds: [i32; 4],
    pattern: Option<HWND>,
    requests: Sender<()>,
    replies: Receiver<anyhow::Result<isize>>,
    close: Sender<Infallible>,
    painter: Option<thread::JoinHandle<()>>,
}

impl Scene {
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "Centers a small window inside one display's bounds"
    )]
    fn open(display: Display) -> anyhow::Result<Self> {
        let [left, top, right, bottom] = display.bounds;
        let [width, height] = [PATTERN_WIDTH + 2 * MARGIN, PATTERN_HEIGHT + 2 * MARGIN];
        // Even offsets from the display's origin keep every edge in halved frames.
        let x = left + (((right - left - width) / 2) & !1);
        let y = top + (((bottom - top - height) / 2) & !1);
        let [overlay_left, overlay_top, overlay_right, overlay_bottom] = OVERLAY;
        let [pattern_x, pattern_y] = [x + MARGIN, y + MARGIN];
        let overlay = Display {
            monitor: display.monitor,
            bounds: [
                pattern_x + overlay_left,
                pattern_y + overlay_top,
                pattern_x + overlay_right,
                pattern_y + overlay_bottom,
            ],
        };
        let (requests, requested) = async_channel::bounded(1);
        let (replying, replies) = async_channel::bounded(3);
        let (close, closing) = async_channel::bounded::<Infallible>(1);
        let layout = [x, y, width, height];
        let painter =
            thread::spawn(move || paint(layout, overlay, &replying, &requested, &closing));
        let backdrop = replies.recv_blocking()??;
        let overlay_window = replies.recv_blocking()??;
        Ok(Self {
            backdrop: backdrop as HWND,
            overlay: overlay_window as HWND,
            overlay_bounds: overlay.bounds,
            pattern: None,
            requests,
            replies,
            close,
            painter: Some(painter),
        })
    }

    /// Has the painter open the pattern window, and waits until it shows.
    fn open_pattern(&mut self) -> anyhow::Result<HWND> {
        self.requests.try_send(())?;
        let pattern = self.replies.recv_blocking()?? as HWND;
        self.pattern = Some(pattern);
        Ok(pattern)
    }

    fn is_ours(&self, window: HWND) -> bool {
        window == self.backdrop || window == self.overlay || Some(window) == self.pattern
    }

    /// Confirms `configure_overlay` placed, styled, and excluded the overlay for good.
    fn check_overlay(&self) -> anyhow::Result<()> {
        let mut bounds = RECT::default();
        // SAFETY: each call reads a window by handle into a live local; a window the
        // painter has destroyed fails it.
        let (measured, styles, visible, marked) = unsafe {
            (
                GetWindowRect(self.overlay, &raw mut bounds),
                GetWindowLongPtrW(self.overlay, GWL_EXSTYLE),
                IsWindowVisible(self.overlay) != FALSE,
                !GetPropW(self.overlay, ALWAYS_EXCLUDED).is_null(),
            )
        };
        ok_or_last_error(measured)?;
        let RECT {
            left,
            top,
            right,
            bottom,
        } = bounds;
        assert_eq!([left, top, right, bottom], self.overlay_bounds);
        assert_eq!(affinity(self.overlay), Some(WDA_EXCLUDEFROMCAPTURE));
        let wanted =
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        assert_eq!(u32::try_from(styles)? & wanted, wanted);
        assert!(visible && marked);
        Ok(())
    }

    /// One frame of `target` from a capture that leaves no window out, kept only where it
    /// shows the backdrop, after proving before and after that nothing else covers it.
    fn unfiltered(&self, target: Target, longest_edge: u32) -> anyhow::Result<Crop> {
        let area = self.area()?;
        self.check_owned(area)?;
        let display = target.display().native;
        let crop = Crop::of(unfiltered_frame(target, longest_edge)?, display, area)?;
        self.check_owned(area)?;
        Ok(crop)
    }

    /// Every frame of a `ScreenCapture` that opens the pattern after its first frame,
    /// kept only where they show the backdrop, with when the pattern opened.
    fn record_opening_pattern(
        &mut self,
        display: crate::screen::Display,
    ) -> anyhow::Result<(Vec<Crop>, Instant)> {
        let area = self.area()?;
        self.check_owned(area)?;
        let (events, frames) = async_channel::bounded(4);
        let started = Instant::now();
        let target = Target::screen()?;
        ensure!(
            target.display() == display,
            "The pointer moved to another display; nothing was inspected"
        );
        let capture = ScreenCapture::start(target, INTERVAL, u32::MAX, events)?;
        let patience = started.checked_add(PATIENCE).unwrap();
        let first = next_event(&frames, patience).context("No frame arrived")?;
        let latency = started.elapsed().as_secs_f64() * 1e3;
        println!("First frame {latency:.1} ms after start");
        let mut crops = vec![Crop::of(frame_of(first)?, display.native, area)?];
        let opened = Instant::now();
        let pattern = self.open_pattern()?;
        let watched = opened.checked_add(WATCH).unwrap();
        while let Some(event) = next_event(&frames, watched) {
            crops.push(Crop::of(frame_of(event)?, display.native, area)?);
        }
        ensure!(
            affinity(pattern) == Some(WDA_EXCLUDEFROMCAPTURE),
            "The capture did not leave out a window opened while it ran"
        );
        drop(capture);
        self.check_owned(area)?;
        Ok((crops, opened))
    }

    fn area(&self) -> anyhow::Result<RECT> {
        let mut bounds = RECT::default();
        // SAFETY: reads a window by handle into a live RECT of the size passed; a window
        // the painter has destroyed fails the call.
        let read = unsafe {
            DwmGetWindowAttribute(
                self.backdrop,
                DWMWA_EXTENDED_FRAME_BOUNDS.cast_unsigned(),
                (&raw mut bounds).cast(),
                u32::try_from(size_of::<RECT>())?,
            )
        };
        ensure!(read >= 0, "Cannot locate the test scene");
        Ok(bounds)
    }

    /// Proves only the scene shows within `area`: a probe every 16 pixels and along the
    /// far edges finds one of its windows, and no visible window above the backdrop
    /// overlaps the area, including click-through windows the probe passes over.
    fn check_owned(&self, area: RECT) -> anyhow::Result<()> {
        let xs = (area.left..area.right)
            .step_by(16)
            .chain([area.right.saturating_sub(1)]);
        let ys = (area.top..area.bottom)
            .step_by(16)
            .chain([area.bottom.saturating_sub(1)]);
        let probed = ys
            .flat_map(|y| xs.clone().map(move |x| POINT { x, y }))
            .all(|point| {
                // SAFETY: both calls take values and retain nothing.
                self.is_ours(unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) })
            });
        let mut covering = top_level_windows()?
            .into_iter()
            .take_while(|&window| window != self.backdrop)
            .filter(|&window| !self.is_ours(window) && shows_over(window, area));
        ensure!(
            probed && covering.next().is_none(),
            "Another window covers the test scene"
        );
        Ok(())
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        self.close.close();
        if let Some(painter) = self.painter.take() {
            // A panicking painter has already failed the test, and its windows died with
            // its thread.
            drop(painter.join());
        }
    }
}

/// Whether `window` is visible on this virtual desktop over `area`, other than the
/// handwriting canvas.
fn shows_over(window: HWND, area: RECT) -> bool {
    let mut cloaked = 0_u32;
    let mut bounds = RECT::default();
    let mut class = [0_u16; 64];
    // SAFETY: each call reads a window by handle into a live local of the size passed.
    let (visible, cloak_read, measured, length) = unsafe {
        (
            IsWindowVisible(window) != FALSE,
            DwmGetWindowAttribute(
                window,
                DWMWA_CLOAKED.cast_unsigned(),
                (&raw mut cloaked).cast(),
                4,
            ),
            GetWindowRect(window, &raw mut bounds) != FALSE,
            GetClassNameW(window, class.as_mut_ptr(), 64),
        )
    };
    let class = String::from_utf16_lossy(&class[..usize::try_from(length).unwrap_or(0)]);
    visible
        && (cloak_read < 0 || cloaked == 0)
        && measured
        && !class.starts_with(HANDWRITING_CANVAS)
        && bounds.left < area.right
        && area.left < bounds.right
        && bounds.top < area.bottom
        && area.top < bounds.bottom
}

/// Runs the scene's windows on this thread, replying with each as it opens, or with what
/// stopped them.
fn paint(
    layout: [i32; 4],
    overlay: Display,
    replies: &Sender<anyhow::Result<isize>>,
    requests: &Receiver<()>,
    closing: &Receiver<Infallible>,
) {
    if let Err(error) = paint_scene(layout, overlay, replies, requests, closing) {
        // A closed lane means the test has already given up on the scene.
        drop(replies.try_send(Err(error)));
    }
}

/// Opens the scene's windows, and the pattern when asked, then keeps them painted until
/// `closing` closes or time runs out.
fn paint_scene(
    [x, y, width, height]: [i32; 4],
    overlay: Display,
    replies: &Sender<anyhow::Result<isize>>,
    requests: &Receiver<()>,
    closing: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    // SAFETY: each brush becomes its class's, which Windows deletes on unregistering.
    let [gray, magenta] =
        [0x0060_6060, 0x00FF_00FF].map(|color| unsafe { CreateSolidBrush(color) });
    let _classes = [
        Class::register(BACKDROP_CLASS, Some(DefWindowProcW), gray)?,
        Class::register(OVERLAY_CLASS, Some(DefWindowProcW), magenta)?,
        Class::register(PATTERN_CLASS, Some(paint_pattern), null_mut())?,
    ];
    let topmost = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let backdrop = Window::create(BACKDROP_CLASS, topmost, [x, y, width, height])?;
    set_affinity(backdrop.0, WDA_MONITOR)?;
    backdrop.show();
    replies.try_send(Ok(backdrop.0 as isize))?;
    let overlay_window = Window::create(
        OVERLAY_CLASS,
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        [0, 0, 1, 1],
    )?;
    // SAFETY: this thread owns the live window until `overlay_window` drops.
    let handle = unsafe { native_window(overlay_window.0) }?;
    configure_overlay(&handle, overlay)?;
    set_overlay_visible(&handle, true);
    replies.try_send(Ok(overlay_window.0 as isize))?;
    let mut pattern = None;
    let deadline = Instant::now().checked_add(ON_SCREEN).unwrap();
    let mut message = MSG::default();
    while !closing.is_closed() && Instant::now() < deadline {
        if pattern.is_none() && requests.try_recv().is_ok() {
            let [pattern_x, pattern_y] = [x, y].map(|edge| edge.saturating_add(MARGIN));
            let bounds = [pattern_x, pattern_y, PATTERN_WIDTH, PATTERN_HEIGHT];
            let window = Window::create(PATTERN_CLASS, topmost, bounds)?;
            window.show();
            // Back above the pattern, as the overlay stays above everything it inks.
            set_overlay_visible(&handle, true);
            replies.try_send(Ok(window.0 as isize))?;
            pattern = Some(window);
        }
        // SAFETY: messages come from this thread's queue and reach only its windows.
        unsafe {
            while PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_REMOVE) != FALSE {
                TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

struct Class(PCWSTR);

impl Class {
    fn register(name: PCWSTR, procedure: WNDPROC, background: HBRUSH) -> anyhow::Result<Self> {
        let class = WNDCLASSW {
            lpfnWndProc: procedure,
            // SAFETY: a null module name selects this executable.
            hInstance: unsafe { GetModuleHandleW(null()) },
            hbrBackground: background,
            lpszClassName: name,
            ..Default::default()
        };
        // SAFETY: the class names a static string and a procedure that live for the
        // process.
        let registered = unsafe { RegisterClassW(&raw const class) } != 0;
        ensure!(registered, "Cannot register a test window class");
        Ok(Self(name))
    }
}

impl Drop for Class {
    fn drop(&mut self) {
        // SAFETY: this thread registered the class and destroyed its windows first.
        unsafe { UnregisterClassW(self.0, GetModuleHandleW(null())) };
    }
}

struct Window(HWND);

impl Window {
    fn create(class: PCWSTR, styles: u32, [x, y, width, height]: [i32; 4]) -> anyhow::Result<Self> {
        // SAFETY: the class is registered on this thread and every pointer is static or
        // null.
        let hwnd = unsafe {
            CreateWindowExW(
                styles,
                class,
                class,
                WS_POPUP,
                x,
                y,
                width,
                height,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        ensure!(!hwnd.is_null(), "Cannot create a test window");
        Ok(Self(hwnd))
    }

    fn show(&self) {
        // SAFETY: shows this thread's own window without activating it.
        unsafe { ShowWindow(self.0, SW_SHOWNOACTIVATE) };
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: this thread created the window and destroys it exactly once.
        unsafe { DestroyWindow(self.0) };
    }
}

unsafe extern "system" fn paint_pattern(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message != WM_PAINT {
        // SAFETY: forwards unchanged arguments for this thread's own window.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let pixels: Vec<u32> = (0..PATTERN_HEIGHT)
        .flat_map(|y| (0..PATTERN_WIDTH).map(move |x| pattern(x, y)))
        .map(|[red, green, blue]| u32::from_be_bytes([0, red, green, blue]))
        .collect();
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: HEADER_SIZE,
            biWidth: PATTERN_WIDTH,
            // Negative heights describe rows from the top down.
            biHeight: -PATTERN_HEIGHT,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: the paint cycle, pixels, and description outlive these synchronous calls
    // on the window's own thread.
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let context = BeginPaint(hwnd, &raw mut paint);
        SetDIBitsToDevice(
            context,
            0,
            0,
            PATTERN_WIDTH.cast_unsigned(),
            PATTERN_HEIGHT.cast_unsigned(),
            0,
            0,
            0,
            PATTERN_HEIGHT.cast_unsigned(),
            pixels.as_ptr().cast(),
            &raw const info,
            DIB_RGB_COLORS,
        );
        EndPaint(hwnd, &raw const paint);
    }
    0
}
