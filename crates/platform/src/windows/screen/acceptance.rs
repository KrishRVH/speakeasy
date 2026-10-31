//! Checks on this machine's GPU and display. The capture check shows Speakeasy's own windows
//! for a few seconds and tallies captured pixels only after proving, before and after each
//! capture, that no other window covers them; it prints nothing but counts and timings.

mod geometry;
mod movie;

use std::{
    io,
    ptr::{null, null_mut},
};

use anyhow::{anyhow, bail};
use windows::Win32::Graphics::Direct3D11::D3D11_SUBRESOURCE_DATA;
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, TRUE, WPARAM},
        Graphics::{
            Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute},
            Gdi::{
                BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, ClientToScreen, CreateSolidBrush,
                DIB_RGB_COLORS, EndPaint, EnumDisplayMonitors, HBRUSH, HDC,
                MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT, SetDIBitsToDevice,
            },
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{
                AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                GetDpiAwarenessContextForProcess, SetProcessDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE,
                GetClientRect, GetWindowLongW, GetWindowRect, IsWindowVisible, MSG,
                NCCALCSIZE_PARAMS, PM_REMOVE, PeekMessageW, RegisterClassW, SW_SHOWMINNOACTIVE,
                SWP_NOZORDER, SetWindowDisplayAffinity, TranslateMessage, UnregisterClassW,
                WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WDA_NONE, WM_DPICHANGED, WM_NCCALCSIZE,
                WM_PAINT, WNDCLASSW, WNDPROC, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
            },
        },
    },
    core::{BOOL, PCWSTR},
    w,
};

use super::{
    exclusion::{affinity, excluded_for_good, top_level_windows},
    readback::{BYTES_PER_PIXEL, created, texture_description},
    *,
};
use crate::{ScreenCapture, windows::native_window};

const PATTERN_WIDTH: i32 = 640;
const PATTERN_HEIGHT: i32 = 400;
/// The first row of the checkerboard band beneath the pattern's quadrants.
const BAND_TOP: i32 = 320;
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
/// How long a capture may take to deliver its first frame.
const FIRST_FRAME: Duration = Duration::from_secs(3);
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
const SUBJECT_CLASS: PCWSTR = w!("SpeakeasyCaptureSubject");
/// GPUI's client-area insets, left, top, right, and bottom, for a window without a title bar on
/// Windows 11 at 96 DPI: a resize border's width on three sides and one pixel on top.
const GPUI_INSETS: [i32; 4] = [8, 1, 8, 8];

/// The pattern's RGB color at `[x, y]`: four quadrants above a checkerboard band. Every
/// edge falls on an even pixel, so halving reproduces it exactly.
fn pattern(x: i32, y: i32) -> [u8; 3] {
    if y >= BAND_TOP {
        return if ((x >> 3) ^ (y >> 3)) & 1 == 0 {
            [255; 3]
        } else {
            BLACK
        };
    }
    match (x < PATTERN_WIDTH / 2, y < BAND_TOP / 2) {
        (true, true) => [220, 40, 40],
        (false, true) => [40, 180, 60],
        (true, false) => [40, 80, 220],
        (false, false) => [230, 200, 40],
    }
}

/// Which of the scene's windows above the backdrop a capture includes.
#[derive(Clone, Copy)]
enum Shown {
    Neither,
    Overlay,
    Pattern,
}

impl Shown {
    /// What capture shows at `[x, y]` of the backdrop: black, as Windows shows the backdrop to
    /// capture, beneath the window it includes.
    fn color(self, x: i32, y: i32) -> [u8; 3] {
        let [x, y] = [x.saturating_sub(MARGIN), y.saturating_sub(MARGIN)];
        let [left, top, right, bottom] = OVERLAY;
        let on_overlay = (left..right).contains(&x) && (top..bottom).contains(&y);
        let on_pattern = (0..PATTERN_WIDTH).contains(&x) && (0..PATTERN_HEIGHT).contains(&y);
        match self {
            Self::Overlay if on_overlay => MAGENTA,
            Self::Pattern if on_pattern => pattern(x, y),
            _ => BLACK,
        }
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
    assert_eq!(before.mismatches(Shown::Overlay), 0, "Unfiltered, before");

    let (during, opened) = scene.record_opening_pattern(display)?;
    let pattern = scene.pattern.context("Missing pattern window")?;
    assert!(
        during.iter().any(|crop| crop.at > opened),
        "No frame arrived after the pattern opened"
    );
    for crop in &during {
        assert_eq!(crop.mismatches(Shown::Neither), 0, "During the capture");
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
    assert_eq!(after.mismatches(Shown::Pattern), 0, "Unfiltered, after");
    let [width, height] = display.native.extent();
    let half = width.max(height).cast_unsigned() / 2;
    let halved = scene.unfiltered(whole(display), half)?;
    assert_eq!(halved.scale, 2);
    assert_eq!(halved.mismatches(Shown::Pattern), 0, "Halved, after");

    let own = top_level_windows()?
        .into_iter()
        .filter(|&window| is_own_window(window))
        .count();
    let mut exclusions = Exclusions::default();
    exclusions.refresh()?;
    let started = Instant::now();
    let refreshes = 100;
    for _ in 0..refreshes {
        exclusions.refresh()?;
    }
    let each = started.elapsed() / refreshes;
    println!("Re-checking {own} own windows: {each:.1?}");
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
    let part = display
        .native
        .bounds()
        .part_covered_by(Bounds::from(scene.area()?))
        .context("The scene lies off its display")?;
    let window = Window(scene.backdrop as isize);
    for region in [Region::Window(window, part), Region::Area(part)] {
        let target = Target { display, region };
        let crop = scene.unfiltered(target, u32::MAX)?;
        assert_eq!(crop.mismatches(Shown::Overlay), 0, "{region:?}");
        assert_eq!(crop.part, part, "{region:?}");
        assert_eq!(
            crop.width, crop.frame_width,
            "{region:?} read back only its part"
        );
    }
    Ok(())
}

#[test]
#[ignore = "Shows an owned window briefly and takes it off screen each way a Show can lose it; inspects no pixels"]
fn a_window_capture_stops_once_its_window_closes_or_goes_off_screen() -> anyhow::Result<()> {
    per_monitor_aware()?;
    join_lasting_apartment()?;
    let display = display_under_pointer()?;
    // SAFETY: takes a color by value; the brush becomes the class's, which Windows deletes on
    // unregistering.
    let gray = unsafe { CreateSolidBrush(0x0060_6060) };
    let _class = Class::register(SUBJECT_CLASS, Some(DefWindowProcW), gray)?;
    let bounds = Layout::centered_on(display.native).pattern;
    for (departure, gone) in [
        (Departure::Hide, WindowGone::OffScreen),
        (Departure::Minimize, WindowGone::OffScreen),
        (Departure::Cloak, WindowGone::OffScreen),
        (Departure::MoveAway, WindowGone::OffScreen),
        (Departure::Close, WindowGone::Closed),
    ] {
        let window = Popup::create(
            SUBJECT_CLASS,
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            bounds,
        )?;
        window.show();
        dispatch_messages_for(COMPOSED);
        let followed = Window(window.0 as isize);
        let part = window_part(display.native, followed)
            .map_err(|gone| anyhow!(gone))?
            .context("Cannot read the owned window's frame")?;
        let target = Target {
            display,
            region: Region::Window(followed, part),
        };
        let (_stop, stopping) = async_channel::bounded::<Infallible>(1);
        let started = Instant::now();
        let mut capture = Capture::start(target, INTERVAL, u32::MAX, &stopping)?;
        read_next(&mut capture, started)?.context("No frame arrived")?;
        // Any frame composed from here on may show the change, so none of them may be read.
        let departing = Instant::now();
        let _remaining = departure.take_off_screen(window)?;
        let stopped = read_next(&mut capture, departing);
        assert!(
            matches!(stopped, Err(Failure::Window(reason)) if reason == gone),
            "{departure:?} left capture at {stopped:?}"
        );
    }
    Ok(())
}

/// A way a recorded window leaves the screen.
#[derive(Clone, Copy, Debug)]
enum Departure {
    Hide,
    Minimize,
    /// As DWM keeps a window on another virtual desktop off screen.
    Cloak,
    MoveAway,
    Close,
}

impl Departure {
    /// Takes `window` off screen this way, returning it unless it closed.
    fn take_off_screen(self, window: Popup) -> anyhow::Result<Option<Popup>> {
        let hwnd = window.0;
        match self {
            // SAFETY: hides this thread's own live window.
            Self::Hide => unsafe {
                ShowWindow(hwnd, SW_HIDE);
            },
            // SAFETY: minimizes this thread's own live window without activating another.
            Self::Minimize => unsafe {
                ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
            },
            Self::Cloak => {
                let cloaked = TRUE;
                // SAFETY: cloaks this process's own window, reading the flag from a live local of
                // the size given.
                HRESULT(unsafe {
                    DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_CLOAK.cast_unsigned(),
                        (&raw const cloaked).cast(),
                        u32::try_from(size_of_val(&cloaked))?,
                    )
                })
                .ok()?;
            },
            // SAFETY: moves this thread's own window where minimized windows wait, off every
            // display.
            Self::MoveAway => ok_or_last_error(unsafe {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    -32_000,
                    -32_000,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                )
            })?,
            Self::Close => return Ok(None),
        }
        Ok(Some(window))
    }
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
        let quarter = (source.map(|edge| edge / 4), source.map(|edge| edge / 2));
        for (origin @ [left, top], size) in [([0, 0], source), quarter] {
            for longest_edge in [u32::MAX, 2560, 960] {
                let readback = Readback::new(&device, size, longest_edge)?;
                let bgra = readback.read(&context, &frame, origin)?;
                let [width, height] = readback.size();
                let level = readback.level();
                let expected: Vec<u8> = (0..height)
                    .flat_map(|y| (0..width).map(move |x| [x, y]))
                    .flat_map(|[x, y]| block_color(left + (x << level), top + (y << level)))
                    .collect();
                assert!(
                    bgra == expected,
                    "{size:?} at {origin:?} of {source:?} read back at {:?}",
                    readback.size()
                );
                let [median, slowest] = cost(&readback, &context, &frame, origin)?;
                println!(
                    "{size:?} → {:?}: median {median:.2?}, slowest {slowest:.2?}",
                    readback.size()
                );
            }
        }
    }
    Ok(())
}

/// The median and slowest of `READBACKS` readbacks of `frame` at `origin`.
fn cost(
    readback: &Readback,
    context: &ID3D11DeviceContext,
    frame: &ID3D11Texture2D,
    origin: [u32; 2],
) -> windows::core::Result<[Duration; 2]> {
    const READBACKS: usize = 30;
    let mut costs = Vec::with_capacity(READBACKS);
    for _ in 0..READBACKS {
        let started = Instant::now();
        readback.read(context, frame, origin)?;
        costs.push(started.elapsed());
    }
    costs.sort();
    Ok([costs[READBACKS / 2], costs[READBACKS - 1]])
}

/// Synthetic frames are constant across 4×4 blocks, so the first two halvings are exact.
fn block_color(x: u32, y: u32) -> [u8; 4] {
    let [x, y] = [x >> 2, y >> 2].map(|block| block.to_le_bytes()[0]);
    [x, y, x ^ y, 255]
}

fn synthetic_frame(
    device: &ID3D11Device,
    [width, height]: [u32; 2],
) -> windows::core::Result<ID3D11Texture2D> {
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
    created(texture)
}

#[test]
#[ignore = "Places a hidden owned window over each display scale; captures nothing"]
fn overlay_covers_its_display_edge_to_edge_across_scales() -> anyhow::Result<()> {
    per_monitor_aware()?;
    let displays = displays()?;
    let first = displays.first().context("No display")?;
    let _class = Class::register(RESCALING_CLASS, Some(rescale_like_gpui), null_mut())?;
    let [left, top, ..] = first.native.edges;
    let window = Popup::create(
        RESCALING_CLASS,
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        [left, top, left.saturating_add(100), top.saturating_add(100)],
    )?;
    // SAFETY: this thread owns the live window until `window` drops.
    let native = unsafe { native_window(window.0) }?;
    configure_overlay(&native, first.native)?;
    assert_eq!(
        covered(window.0)?,
        [first.native.edges; 2],
        "The window and its client area, where ink is drawn, cover the display"
    );
    let scale = |display: &screen::Display| display.native.extent()[0] as f32 / display.size[0];
    let Some(to) = displays
        .iter()
        .find(|to| (scale(to) - scale(first)).abs() > 0.01)
    else {
        let scales: Vec<f32> = displays.iter().map(scale).collect();
        println!("Crossing scales needs two displays of different scale, not {scales:?}");
        return Ok(());
    };
    // SAFETY: reads this thread's own live window.
    let dpi = || unsafe { GetDpiForWindow(window.0) };
    let before = dpi();
    configure_overlay(&native, to.native)?;
    assert_ne!(before, dpi(), "The window must arrive at another scale");
    assert_eq!(covered(window.0)?, [to.native.edges; 2]);
    Ok(())
}

/// The left, top, right, and bottom edges of `window` and of its client area, on the desktop.
fn covered(window: HWND) -> anyhow::Result<[[i32; 4]; 2]> {
    let (mut frame, mut client, mut origin) = (RECT::default(), RECT::default(), POINT::default());
    // SAFETY: reads this thread's own live window into live locals.
    unsafe {
        ok_or_last_error(GetWindowRect(window, &raw mut frame))?;
        ok_or_last_error(GetClientRect(window, &raw mut client))?;
        ok_or_last_error(ClientToScreen(window, &raw mut origin))?;
    }
    Ok([
        edges(frame),
        [
            origin.x,
            origin.y,
            origin.x.saturating_add(client.right),
            origin.y.saturating_add(client.bottom),
        ],
    ])
}

/// Every display on the desktop.
fn displays() -> anyhow::Result<Vec<screen::Display>> {
    type Monitor = windows_sys::Win32::Graphics::Gdi::HMONITOR;
    unsafe extern "system" fn collect(
        monitor: Monitor,
        _: HDC,
        _: *mut RECT,
        monitors: LPARAM,
    ) -> BOOL {
        // SAFETY: `displays` passes its live vector for the length of the enumeration.
        unsafe { (*(monitors as *mut Vec<Monitor>)).push(monitor) };
        TRUE
    }
    let mut monitors: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only pushes into the vector, which outlives the enumeration.
    ok_or_last_error(unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect),
            (&raw mut monitors) as LPARAM,
        )
    })?;
    monitors.into_iter().map(describe).collect()
}

/// Answers a DPI change as GPUI does, by taking the size and place Windows suggests, and keeps
/// `GPUI_INSETS` out of the client area.
unsafe extern "system" fn rescale_like_gpui(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCALCSIZE if wparam != 0 => {
            // SAFETY: for WM_NCCALCSIZE with wparam set, lparam points to this call's
            // NCCALCSIZE_PARAMS, whose first rectangle is the proposed window rectangle.
            let proposed = unsafe { &mut (*(lparam as *mut NCCALCSIZE_PARAMS)).rgrc[0] };
            let [left, top, right, bottom] = GPUI_INSETS;
            proposed.left = proposed.left.saturating_add(left);
            proposed.top = proposed.top.saturating_add(top);
            proposed.right = proposed.right.saturating_sub(right);
            proposed.bottom = proposed.bottom.saturating_sub(bottom);
            0
        },
        WM_DPICHANGED => {
            // SAFETY: for WM_DPICHANGED, lparam points to the suggested RECT for this call.
            let [left, top, right, bottom] = edges(unsafe { *(lparam as *const RECT) });
            let [width, height] = [right.saturating_sub(left), bottom.saturating_sub(top)];
            // SAFETY: moves this thread's own window.
            unsafe {
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    left,
                    top,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            0
        },
        // SAFETY: forwards unchanged arguments for this thread's own window.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
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

fn set_affinity(window: HWND, affinity: u32) -> io::Result<()> {
    // SAFETY: takes a window of this process by value.
    ok_or_last_error(unsafe { SetWindowDisplayAffinity(window, affinity) })
}

/// All of `display`.
const fn whole(display: screen::Display) -> Target {
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
    read_next(&mut capture, started)?.context("No frame arrived")
}

/// The first frame Windows composes from `since` on that `capture` reads back within
/// [`FIRST_FRAME`], or what stopped it.
fn read_next(capture: &mut Capture, since: Instant) -> Result<Option<Frame>, Failure> {
    let deadline = Instant::now().checked_add(FIRST_FRAME).unwrap();
    while Instant::now() < deadline {
        if capture.poll(since)?
            && let Some(frame) = capture.read_pending()?
        {
            return Ok(Some(frame));
        }
        thread::sleep(POLL);
    }
    Ok(None)
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
        ScreenEvent::WindowGone(gone) => bail!(gone),
        ScreenEvent::Failed(message) => bail!(message),
        ScreenEvent::MovieStopped => bail!("Synthetic movie stopped"),
    }
}

/// The pixels of the scene's backdrop from one captured frame, `scale` physical pixels
/// apiece.
struct Crop {
    scale: usize,
    width: usize,
    at: Instant,
    bgra: Vec<[u8; 4]>,
    /// The part of the display the frame showed.
    part: Part,
    /// The frame's width in pixels.
    frame_width: usize,
}

impl Crop {
    /// Keeps only the part of `frame` showing `area`, then drops the frame.
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "Offsets within one captured display, bounded by indexing"
    )]
    fn of(frame: Frame, display: Display, area: RECT) -> anyhow::Result<Self> {
        let [display_left, display_top, ..] = display.edges;
        let extent = display.extent().map(i32::cast_unsigned);
        let [left, top, right, bottom] = pixel_box(frame.part, extent).map(u32::cast_signed);
        let [width, height] = [right - left, bottom - top];
        let [left, top] = [display_left + left, display_top + top];
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
        let frame_width = frame.width as usize;
        let (pixels, _) = frame.bgra.as_chunks::<BYTES_PER_PIXEL>();
        let bgra = pixels
            .chunks_exact(frame_width)
            .skip(row)
            .take(crop_height)
            .flat_map(|line| &line[column..column + crop_width])
            .copied()
            .collect();
        Ok(Self {
            scale: scale.cast_unsigned() as usize,
            width: crop_width,
            at: frame.at,
            bgra,
            part: frame.part,
            frame_width,
        })
    }

    /// How many pixels stray from what capture shows at each physical offset into the crop
    /// when it includes `shown`.
    fn mismatches(&self, shown: Shown) -> usize {
        let positions = || (0..).step_by(self.scale);
        self.bgra
            .chunks_exact(self.width)
            .zip(positions())
            .map(|(line, y)| {
                line.iter()
                    .zip(positions())
                    .filter(|&(&pixel, x)| !near(pixel, shown.color(x, y)))
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
    overlay_edges: [i32; 4],
    pattern: Option<HWND>,
    requests: Sender<()>,
    replies: Receiver<anyhow::Result<isize>>,
    close: Sender<Infallible>,
    painter: Option<thread::JoinHandle<()>>,
}

impl Scene {
    fn open(display: Display) -> anyhow::Result<Self> {
        let layout = Layout::centered_on(display);
        let (requests, requested) = async_channel::bounded(1);
        let (replying, replies) = async_channel::bounded(3);
        let (close, closing) = async_channel::bounded::<Infallible>(1);
        let painter = thread::spawn(move || paint(layout, &replying, &requested, &closing));
        let backdrop = replies.recv_blocking()??;
        let overlay = replies.recv_blocking()??;
        Ok(Self {
            backdrop: backdrop as HWND,
            overlay: overlay as HWND,
            overlay_edges: layout.overlay.edges,
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
        // SAFETY: reads a window by handle into a live local; a window the painter has
        // destroyed fails the call.
        ok_or_last_error(unsafe { GetWindowRect(self.overlay, &raw mut bounds) })?;
        assert_eq!(edges(bounds), self.overlay_edges);
        assert_eq!(affinity(self.overlay), Some(WDA_EXCLUDEFROMCAPTURE));
        // SAFETY: both calls read a window by handle.
        let (styles, visible) = unsafe {
            (
                GetWindowLongW(self.overlay, GWL_EXSTYLE).cast_unsigned(),
                IsWindowVisible(self.overlay) != FALSE,
            )
        };
        let wanted =
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        assert_eq!(styles & wanted, wanted);
        assert!(visible, "The overlay is hidden");
        assert!(
            excluded_for_good(self.overlay),
            "The overlay lacks the mark that keeps every capture from restoring it"
        );
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
        display: screen::Display,
    ) -> anyhow::Result<(Vec<Crop>, Instant)> {
        let area = self.area()?;
        self.check_owned(area)?;
        let (events, frames) = async_channel::bounded(4);
        let started = Instant::now();
        let target = Target::screen_under_pointer()?;
        ensure!(
            target.display() == display,
            "The pointer moved to another display; nothing was inspected"
        );
        let capture = ScreenCapture::start(target, INTERVAL, u32::MAX, events, None)?;
        let deadline = started.checked_add(FIRST_FRAME).unwrap();
        let first = next_event(&frames, deadline).context("No frame arrived")?;
        println!("First frame {:.1?} after start", started.elapsed());
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
        frame_bounds(self.backdrop).context("Cannot locate the test scene")
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
            .all(|point| top_level_at(point).is_some_and(|window| self.is_ours(window)));
        let covered = top_level_windows()?
            .into_iter()
            .take_while(|&window| window != self.backdrop)
            .any(|window| !self.is_ours(window) && shows_over(window, area));
        ensure!(probed && !covered, "Another window covers the test scene");
        Ok(())
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        self.close.close();
        if let Some(painter) = self.painter.take() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A panicking painter has already failed the test, and its windows died with its thread"
            )]
            let _ = painter.join();
        }
    }
}

/// Whether `window` is visible on this virtual desktop over `area`, other than the
/// handwriting canvas.
fn shows_over(window: HWND, area: RECT) -> bool {
    let mut bounds = RECT::default();
    // SAFETY: both calls read a window by handle, into a live local for the bounds.
    let (visible, measured) = unsafe {
        (
            IsWindowVisible(window) != FALSE,
            GetWindowRect(window, &raw mut bounds) != FALSE,
        )
    };
    visible
        && !is_cloaked(window)
        && measured
        && !class_name(window).starts_with(HANDWRITING_CANVAS)
        && bounds.left < area.right
        && area.left < bounds.right
        && bounds.top < area.bottom
        && area.top < bounds.bottom
}

/// Where the painter places the scene's windows, each by its left, top, right, and bottom edges.
#[derive(Clone, Copy)]
struct Layout {
    backdrop: [i32; 4],
    pattern: [i32; 4],
    /// A display of exactly the overlay's bounds, which `configure_overlay` stretches it over.
    overlay: Display,
}

impl Layout {
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "Centers a small window inside one display's bounds"
    )]
    fn centered_on(display: Display) -> Self {
        let [left, top, right, bottom] = display.edges;
        let [width, height] = [PATTERN_WIDTH + 2 * MARGIN, PATTERN_HEIGHT + 2 * MARGIN];
        // Even offsets from the display's origin keep every edge in halved frames.
        let x = left + (((right - left - width) / 2) & !1);
        let y = top + (((bottom - top - height) / 2) & !1);
        let [pattern_x, pattern_y] = [x + MARGIN, y + MARGIN];
        let [overlay_left, overlay_top, overlay_right, overlay_bottom] = OVERLAY;
        Self {
            backdrop: [x, y, x + width, y + height],
            pattern: [
                pattern_x,
                pattern_y,
                pattern_x + PATTERN_WIDTH,
                pattern_y + PATTERN_HEIGHT,
            ],
            overlay: Display {
                monitor: display.monitor,
                edges: [
                    pattern_x + overlay_left,
                    pattern_y + overlay_top,
                    pattern_x + overlay_right,
                    pattern_y + overlay_bottom,
                ],
            },
        }
    }
}

/// Runs the scene's windows on this thread, replying with each as it opens, or with what
/// stopped them.
fn paint(
    layout: Layout,
    replies: &Sender<anyhow::Result<isize>>,
    requests: &Receiver<()>,
    closing: &Receiver<Infallible>,
) {
    if let Err(error) = paint_scene(layout, replies, requests, closing) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A closed lane means the test has already given up on the scene"
        )]
        let _ = replies.try_send(Err(error));
    }
}

/// Opens the scene's windows, and the pattern when asked, then keeps them painted until
/// `closing` closes or time runs out.
fn paint_scene(
    layout: Layout,
    replies: &Sender<anyhow::Result<isize>>,
    requests: &Receiver<()>,
    closing: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    // SAFETY: takes a color by value; each brush becomes its class's, which Windows deletes on
    // unregistering.
    let [gray, magenta] = [[0x60; 3], MAGENTA].map(|[red, green, blue]| unsafe {
        CreateSolidBrush(u32::from_le_bytes([red, green, blue, 0]))
    });
    let _classes = [
        Class::register(BACKDROP_CLASS, Some(DefWindowProcW), gray)?,
        Class::register(OVERLAY_CLASS, Some(DefWindowProcW), magenta)?,
        Class::register(PATTERN_CLASS, Some(paint_pattern), null_mut())?,
    ];
    let topmost = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let backdrop = Popup::create(BACKDROP_CLASS, topmost, layout.backdrop)?;
    set_affinity(backdrop.0, WDA_MONITOR)?;
    backdrop.show();
    replies.try_send(Ok(backdrop.0 as isize))?;
    let overlay = Popup::create(
        OVERLAY_CLASS,
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        [0, 0, 1, 1],
    )?;
    // SAFETY: this thread owns the live window until `overlay` drops.
    let native = unsafe { native_window(overlay.0) }?;
    configure_overlay(&native, layout.overlay)?;
    set_overlay_visible(&native, true);
    replies.try_send(Ok(overlay.0 as isize))?;
    let mut pattern = None;
    let deadline = Instant::now().checked_add(ON_SCREEN).unwrap();
    while !closing.is_closed() && Instant::now() < deadline {
        if pattern.is_none() && requests.try_recv().is_ok() {
            let window = Popup::create(PATTERN_CLASS, topmost, layout.pattern)?;
            window.show();
            // Back above the pattern, as the overlay stays above everything it inks.
            set_overlay_visible(&native, true);
            replies.try_send(Ok(window.0 as isize))?;
            pattern = Some(window);
        }
        dispatch_messages();
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// Delivers the messages waiting for this thread's windows.
fn dispatch_messages() {
    let mut message = MSG::default();
    // SAFETY: messages come from this thread's queue and reach only its windows.
    unsafe {
        while PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_REMOVE) != FALSE {
            TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
}

/// Keeps delivering this thread's window messages for `duration`, so its windows paint.
fn dispatch_messages_for(duration: Duration) {
    let until = Instant::now().checked_add(duration).unwrap();
    while Instant::now() < until {
        dispatch_messages();
        thread::sleep(Duration::from_millis(5));
    }
}

/// A window class this thread registers and unregisters.
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

/// A popup window this thread creates and destroys.
struct Popup(HWND);

impl Popup {
    fn create(
        class: PCWSTR,
        styles: u32,
        [left, top, right, bottom]: [i32; 4],
    ) -> anyhow::Result<Self> {
        let [width, height] = [right.saturating_sub(left), bottom.saturating_sub(top)];
        // SAFETY: the class is registered on this thread and every pointer is static or
        // null.
        let hwnd = unsafe {
            CreateWindowExW(
                styles,
                class,
                class,
                WS_POPUP,
                left,
                top,
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

impl Drop for Popup {
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
