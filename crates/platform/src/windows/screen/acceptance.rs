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

const BAND_TOP: i32 = 320;

const MARGIN: i32 = 32;

const OVERLAY: [i32; 4] = [200, 120, 440, 280];
const BLACK: [u8; 3] = [0; 3];
const MAGENTA: [u8; 3] = [255, 0, 255];

const TOLERANCE: u8 = 6;

const ON_SCREEN: Duration = Duration::from_secs(6);

const COMPOSED: Duration = Duration::from_millis(300);

const FIRST_FRAME: Duration = Duration::from_secs(3);
const INTERVAL: Duration = Duration::from_millis(100);

const WATCH: Duration = Duration::from_secs(1);
#[expect(clippy::cast_possible_truncation, reason = "The header is 40 bytes")]
const HEADER_SIZE: u32 = size_of::<BITMAPINFOHEADER>() as u32;

const HANDWRITING_CANVAS: &str = "ShellHandwritingCanvas";
const BACKDROP_CLASS: PCWSTR = w!("SpeakeasyCaptureBackdrop");
const PATTERN_CLASS: PCWSTR = w!("SpeakeasyCapturePattern");
const OVERLAY_CLASS: PCWSTR = w!("SpeakeasyCaptureOverlay");
const GPUI_POPUP_CLASS: PCWSTR = w!("SpeakeasyGpuiPopup");
const SUBJECT_CLASS: PCWSTR = w!("SpeakeasyCaptureSubject");

const GPUI_POPUP_STYLES: [u32; 2] = [0, WS_EX_TOOLWINDOW | WS_EX_TOPMOST];

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

#[derive(Clone, Copy)]
enum Shown {
    Neither,
    Overlay,
    Pattern,
}

impl Shown {
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

#[derive(Clone, Copy, Debug)]
enum Departure {
    Hide,
    Minimize,
    Cloak,
    MoveAway,
    Close,
}

impl Departure {
    fn take_off_screen(self, window: Popup) -> anyhow::Result<Option<Popup>> {
        let hwnd = window.0;
        match self {
            Self::Hide => unsafe {
                ShowWindow(hwnd, SW_HIDE);
            },
            Self::Minimize => unsafe {
                ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
            },
            Self::Cloak => {
                let cloaked = TRUE;

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
    let primary = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let (device, context) = device_for(HMONITOR(primary))?;
    for source in [[1920, 1080], [3840, 2160]] {
        let frame = synthetic_frame(&device, source)?;

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
    let _class = Class::register(GPUI_POPUP_CLASS, Some(popup_like_gpui), null_mut())?;
    let [left, top, ..] = first.native.edges;
    let window = Popup::styled(
        GPUI_POPUP_CLASS,
        GPUI_POPUP_STYLES,
        [left, top, left.saturating_add(100), top.saturating_add(100)],
    )?;

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

    let dpi = || unsafe { GetDpiForWindow(window.0) };
    let before = dpi();
    configure_overlay(&native, to.native)?;
    assert_ne!(before, dpi(), "The window must arrive at another scale");
    assert_eq!(covered(window.0)?, [to.native.edges; 2]);
    Ok(())
}

fn covered(window: HWND) -> anyhow::Result<[[i32; 4]; 2]> {
    let (mut frame, mut client, mut origin) = (RECT::default(), RECT::default(), POINT::default());

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

fn displays() -> anyhow::Result<Vec<screen::Display>> {
    type Monitor = windows_sys::Win32::Graphics::Gdi::HMONITOR;
    unsafe extern "system" fn collect(
        monitor: Monitor,
        _: HDC,
        _: *mut RECT,
        monitors: LPARAM,
    ) -> BOOL {
        unsafe { (*(monitors as *mut Vec<Monitor>)).push(monitor) };
        TRUE
    }
    let mut monitors: Vec<Monitor> = Vec::new();

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

unsafe extern "system" fn popup_like_gpui(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCALCSIZE if wparam != 0 => {
            let proposed = lparam as *mut NCCALCSIZE_PARAMS;

            unsafe {
                let top = (*proposed).rgrc[0].top;
                let result = DefWindowProcW(hwnd, message, wparam, lparam);
                (*proposed).rgrc[0].top = top;
                result
            }
        },
        WM_DPICHANGED => {
            let [left, top, right, bottom] = edges(unsafe { *(lparam as *const RECT) });
            let [width, height] = [right.saturating_sub(left), bottom.saturating_sub(top)];

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
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn per_monitor_aware() -> anyhow::Result<()> {
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
    ok_or_last_error(unsafe { SetWindowDisplayAffinity(window, affinity) })
}

const fn whole(display: screen::Display) -> Target {
    Target {
        display,
        region: Region::Whole,
    }
}

fn unfiltered_frame(target: Target, longest_edge: u32) -> anyhow::Result<Frame> {
    join_lasting_apartment()?;
    let (_stop, stopping) = async_channel::bounded::<Infallible>(1);
    let started = Instant::now();
    let mut capture = Capture::start(target, INTERVAL, longest_edge, &stopping)?;
    read_next(&mut capture, started)?.context("No frame arrived")
}

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

struct Crop {
    scale: usize,
    width: usize,
    at: Instant,
    bgra: Vec<[u8; 4]>,
    part: Part,
    frame_width: usize,
}

impl Crop {
    fn of(frame: Frame, display: Display, area: RECT) -> anyhow::Result<Self> {
        let [display_left, display_top, ..] = display.edges;
        let extent = display.extent().map(i32::cast_unsigned);
        let [left, top, right, bottom] = pixel_box(frame.part, extent).map(u32::cast_signed);
        let [width, height] = [
            right.checked_sub(left).unwrap(),
            bottom.checked_sub(top).unwrap(),
        ];
        let [left, top] = [
            display_left.checked_add(left).unwrap(),
            display_top.checked_add(top).unwrap(),
        ];
        let scale = width.checked_div(frame.width.cast_signed()).unwrap();
        ensure!(
            scale > 0 && frame.height.cast_signed().checked_mul(scale).unwrap() == height,
            "The frame does not match its part of the display"
        );
        let frame_pixels =
            |physical: i32| physical.checked_div(scale).unwrap().cast_unsigned() as usize;
        let column = frame_pixels(area.left.checked_sub(left).unwrap());
        let row = frame_pixels(area.top.checked_sub(top).unwrap());
        let crop_width = frame_pixels(area.right.checked_sub(area.left).unwrap());
        let crop_height = frame_pixels(area.bottom.checked_sub(area.top).unwrap());
        let end_column = column.checked_add(crop_width).unwrap();
        let frame_width = frame.width as usize;
        let (pixels, _) = frame.bgra.as_chunks::<BYTES_PER_PIXEL>();
        let bgra = pixels
            .chunks_exact(frame_width)
            .skip(row)
            .take(crop_height)
            .flat_map(|line| &line[column..end_column])
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

    fn open_pattern(&mut self) -> anyhow::Result<HWND> {
        self.requests.try_send(())?;
        let pattern = self.replies.recv_blocking()?? as HWND;
        self.pattern = Some(pattern);
        Ok(pattern)
    }

    fn is_ours(&self, window: HWND) -> bool {
        window == self.backdrop || window == self.overlay || Some(window) == self.pattern
    }

    fn check_overlay(&self) -> anyhow::Result<()> {
        let mut bounds = RECT::default();

        ok_or_last_error(unsafe { GetWindowRect(self.overlay, &raw mut bounds) })?;
        assert_eq!(edges(bounds), self.overlay_edges);
        assert_eq!(affinity(self.overlay), Some(WDA_EXCLUDEFROMCAPTURE));

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

    fn unfiltered(&self, target: Target, longest_edge: u32) -> anyhow::Result<Crop> {
        let area = self.area()?;
        self.check_owned(area)?;
        let display = target.display().native;
        let crop = Crop::of(unfiltered_frame(target, longest_edge)?, display, area)?;
        self.check_owned(area)?;
        Ok(crop)
    }

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

fn shows_over(window: HWND, area: RECT) -> bool {
    let mut bounds = RECT::default();

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

#[derive(Clone, Copy)]
struct Layout {
    backdrop: [i32; 4],
    pattern: [i32; 4],
    overlay: Display,
}

impl Layout {
    fn centered_on(display: Display) -> Self {
        let [left, top, right, bottom] = display.edges;
        let [width, height] = [PATTERN_WIDTH + 2 * MARGIN, PATTERN_HEIGHT + 2 * MARGIN];

        let offset_x = right.checked_sub(left).unwrap().checked_sub(width).unwrap() / 2;
        let offset_y = bottom
            .checked_sub(top)
            .unwrap()
            .checked_sub(height)
            .unwrap()
            / 2;
        let x = left.checked_add(offset_x & !1).unwrap();
        let y = top.checked_add(offset_y & !1).unwrap();
        let [pattern_x, pattern_y] = [
            x.checked_add(MARGIN).unwrap(),
            y.checked_add(MARGIN).unwrap(),
        ];
        let [overlay_left, overlay_top, overlay_right, overlay_bottom] = OVERLAY;
        Self {
            backdrop: [
                x,
                y,
                x.checked_add(width).unwrap(),
                y.checked_add(height).unwrap(),
            ],
            pattern: [
                pattern_x,
                pattern_y,
                pattern_x.checked_add(PATTERN_WIDTH).unwrap(),
                pattern_y.checked_add(PATTERN_HEIGHT).unwrap(),
            ],
            overlay: Display {
                monitor: display.monitor,
                edges: [
                    pattern_x.checked_add(overlay_left).unwrap(),
                    pattern_y.checked_add(overlay_top).unwrap(),
                    pattern_x.checked_add(overlay_right).unwrap(),
                    pattern_y.checked_add(overlay_bottom).unwrap(),
                ],
            },
        }
    }
}

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

fn paint_scene(
    layout: Layout,
    replies: &Sender<anyhow::Result<isize>>,
    requests: &Receiver<()>,
    closing: &Receiver<Infallible>,
) -> anyhow::Result<()> {
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
    let overlay = Popup::styled(OVERLAY_CLASS, GPUI_POPUP_STYLES, [0, 0, 1, 1])?;

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

            set_overlay_visible(&native, true);
            replies.try_send(Ok(window.0 as isize))?;
            pattern = Some(window);
        }
        dispatch_messages();
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn dispatch_messages() {
    let mut message = MSG::default();

    unsafe {
        while PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_REMOVE) != FALSE {
            TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
}

fn dispatch_messages_for(duration: Duration) {
    let until = Instant::now().checked_add(duration).unwrap();
    while Instant::now() < until {
        dispatch_messages();
        thread::sleep(Duration::from_millis(5));
    }
}

struct Class(PCWSTR);

impl Class {
    fn register(name: PCWSTR, procedure: WNDPROC, background: HBRUSH) -> anyhow::Result<Self> {
        let class = WNDCLASSW {
            lpfnWndProc: procedure,
            hInstance: unsafe { GetModuleHandleW(null()) },
            hbrBackground: background,
            lpszClassName: name,
            ..Default::default()
        };

        let registered = unsafe { RegisterClassW(&raw const class) } != 0;
        ensure!(registered, "Cannot register a test window class");
        Ok(Self(name))
    }
}

impl Drop for Class {
    fn drop(&mut self) {
        unsafe { UnregisterClassW(self.0, GetModuleHandleW(null())) };
    }
}

struct Popup(HWND);

impl Popup {
    fn create(class: PCWSTR, extended: u32, bounds: [i32; 4]) -> anyhow::Result<Self> {
        Self::styled(class, [WS_POPUP, extended], bounds)
    }

    fn styled(
        class: PCWSTR,
        [style, extended]: [u32; 2],
        [left, top, right, bottom]: [i32; 4],
    ) -> anyhow::Result<Self> {
        let [width, height] = [right.saturating_sub(left), bottom.saturating_sub(top)];

        let hwnd = unsafe {
            CreateWindowExW(
                extended,
                class,
                class,
                style,
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
        unsafe { ShowWindow(self.0, SW_SHOWNOACTIVATE) };
    }
}

impl Drop for Popup {
    fn drop(&mut self) {
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
            biHeight: -PATTERN_HEIGHT,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };

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
