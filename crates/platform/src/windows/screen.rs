use crate::movie::recording as movie;

mod exclusion;
mod layout;
mod readback;

use std::{
    convert::Infallible,
    error, fmt,
    ptr::null_mut,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail, ensure};
use async_channel::{Receiver, Sender};
use windows::{
    Foundation::{TimeSpan, TypedEventHandler},
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess,
            GraphicsCaptureAccessKind, GraphicsCaptureItem, GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus,
    Win32::{
        Graphics::{
            Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D},
            Dxgi::{
                DXGI_ERROR_DEVICE_HUNG, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET,
                DXGI_ERROR_DRIVER_INTERNAL_ERROR, IDXGIDevice,
            },
            Gdi::HMONITOR,
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
        },
    },
    core::{HRESULT, Interface, factory},
};
use windows_future::{AsyncStatus, IAsyncOperation};
use windows_sys::Win32::{
    Foundation::{FALSE, HWND, POINT, RECT},
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
        Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
    },
    System::{
        Com::CoIncrementMTAUsage,
        Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
    },
    UI::{
        HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
        Input::KeyboardAndMouse::{VK_LBUTTON, VK_RBUTTON},
        WindowsAndMessaging::{
            GA_ROOT, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetCursorPos, GetSystemMetrics,
            HWND_TOPMOST, IsIconic, IsWindow, IsWindowVisible, SM_SWAPBUTTON, SW_HIDE,
            SW_SHOWNOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SetWindowPos, ShowWindow, WS_EX_TRANSPARENT, WindowFromPoint,
        },
    },
};

pub(crate) use self::layout::stack;
use self::{
    exclusion::{Exclusions, exclude_always},
    readback::{Readback, device_for},
};
use super::{BASE_DPI, is_key_down, is_own_window, modifiers, ok_or_last_error};
use crate::{
    NativeWindow,
    screen::{
        self, Frame, Part, PointerSample, Region, ScreenEvent, Target, WindowGone,
        geometry::{Bounds, part_of_box, pixel_box},
    },
};

const NO_POINTER: &str = "Cannot find the pointer. Try again.";

const POLL: Duration = Duration::from_millis(10);

const BUFFERS: i32 = 2;

const ACCESS_WAIT: Duration = Duration::from_millis(250);

const POOL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

const TICKS_PER_SECOND: i128 = 10_000_000;

const COMPOSITION_MARGIN: Duration = Duration::from_millis(50);

const SHELL_CLASSES: [&str; 4] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Display {
    monitor: isize,
    edges: [i32; 4],
}

impl Display {
    fn handle(self) -> HMONITOR {
        HMONITOR(self.monitor as *mut _)
    }

    fn extent(self) -> [i32; 2] {
        let [left, top, right, bottom] = self.edges;
        [right.saturating_sub(left), bottom.saturating_sub(top)]
    }

    fn bounds(self) -> Bounds {
        let [left, top, right, bottom] = self.edges;
        Bounds::from(RECT {
            left,
            top,
            right,
            bottom,
        })
    }
}

impl From<RECT> for Bounds {
    fn from(rect: RECT) -> Self {
        let [left, top, right, bottom] = edges(rect).map(f64::from);
        Self {
            origin: [left, top],
            size: [right - left, bottom - top],
        }
    }
}

const fn edges(rect: RECT) -> [i32; 4] {
    [rect.left, rect.top, rect.right, rect.bottom]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window(isize);

impl Window {
    fn handle(self) -> HWND {
        self.0 as HWND
    }
}

pub(crate) fn window_under_pointer(display: Display) -> anyhow::Result<Option<(Window, Part)>> {
    let Some(window) = top_level_at(pointer_location().context(NO_POINTER)?) else {
        return Ok(None);
    };
    if is_own_window(window) || SHELL_CLASSES.contains(&class_name(window).as_str()) {
        return Ok(None);
    }
    let window = Window(window as isize);
    match window_part(display, window) {
        Ok(Some(part)) => Ok(Some((window, part))),
        Ok(None) => bail!("Cannot measure the window under the pointer. Try again."),
        Err(_) => Ok(None),
    }
}

fn top_level_at(point: POINT) -> Option<HWND> {
    let window = unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) };
    (!window.is_null()).then_some(window)
}

fn class_name(window: HWND) -> String {
    let mut class = [0_u16; 256];

    let length = unsafe {
        GetClassNameW(
            window,
            class.as_mut_ptr(),
            i32::try_from(class.len()).unwrap_or_default(),
        )
    };
    let length = usize::try_from(length).unwrap_or_default();
    String::from_utf16_lossy(class.get(..length).unwrap_or_default())
}

fn window_part(display: Display, window: Window) -> Result<Option<Part>, WindowGone> {
    let window = window.handle();

    let (exists, shown) = unsafe {
        (
            IsWindow(window) != FALSE,
            IsWindowVisible(window) != FALSE && IsIconic(window) == FALSE,
        )
    };
    if !exists {
        return Err(WindowGone::Closed);
    }
    if !shown || is_cloaked(window) {
        return Err(WindowGone::OffScreen);
    }
    let Some(frame) = frame_bounds(window) else {
        return Ok(None);
    };
    display
        .bounds()
        .part_covered_by(Bounds::from(frame))
        .map(Some)
        .ok_or(WindowGone::OffScreen)
}

fn is_cloaked(window: HWND) -> bool {
    let mut cloaked = 0_u32;
    let Ok(size) = u32::try_from(size_of_val(&cloaked)) else {
        return false;
    };

    let read = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED.cast_unsigned(),
            (&raw mut cloaked).cast(),
            size,
        )
    };
    read >= 0 && cloaked != 0
}

fn frame_bounds(window: HWND) -> Option<RECT> {
    let mut frame = RECT::default();
    let size = u32::try_from(size_of::<RECT>()).ok()?;

    let read = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_EXTENDED_FRAME_BOUNDS.cast_unsigned(),
            (&raw mut frame).cast(),
            size,
        )
    };
    (read >= 0).then_some(frame)
}

pub(crate) fn display_under_pointer() -> anyhow::Result<screen::Display> {
    let pointer = pointer_location().context(NO_POINTER)?;

    describe(unsafe { MonitorFromPoint(pointer, MONITOR_DEFAULTTONEAREST) })
        .context("Cannot measure the display under the pointer")
}

fn describe(
    monitor: windows_sys::Win32::Graphics::Gdi::HMONITOR,
) -> anyhow::Result<screen::Display> {
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>())?,
        ..Default::default()
    };

    ok_or_last_error(unsafe { GetMonitorInfoW(monitor, &raw mut info) })?;
    let (mut dpi, mut vertical_dpi) = (0, 0);

    let scaled = unsafe {
        GetDpiForMonitor(
            monitor,
            MDT_EFFECTIVE_DPI,
            &raw mut dpi,
            &raw mut vertical_dpi,
        )
    };
    ensure!(scaled >= 0 && dpi > 0, "Cannot read the display's scale");
    let native = Display {
        monitor: monitor as isize,
        edges: edges(info.rcMonitor),
    };
    let scale = dpi as f32 / BASE_DPI as f32;
    Ok(screen::Display {
        native,
        size: native.extent().map(|edge| edge as f32 / scale),
    })
}

fn is_current(display: screen::Display) -> bool {
    describe(display.native.handle().0).is_ok_and(|current| current == display)
}

pub(crate) fn pointer(target: Target, _: crate::MacShortcut) -> Option<PointerSample> {
    let display = target.display.native;
    let part = pointer_part(target)?;
    let at = Instant::now();
    let POINT { x, y } = pointer_location()?;
    Some(PointerSample::Observed {
        at,
        position: display.bounds().normalize([x, y].map(f64::from))?,
        part,
        drawing: modifiers().draws(),
        pressed: primary_button_down(),
    })
}

fn pointer_part(target: Target) -> Option<Part> {
    if !is_current(target.display) {
        return None;
    }
    let part = match target.region {
        Region::Window(window, _) => window_part(target.display.native, window).ok().flatten(),
        _ => Some(target.part()),
    }?;
    let [width, height] = target.display.native.extent();
    let size = [u32::try_from(width).ok()?, u32::try_from(height).ok()?];
    Some(part_of_box(pixel_box(part, size), size))
}

fn pointer_location() -> Option<POINT> {
    let mut point = POINT::default();

    (unsafe { GetCursorPos(&raw mut point) } != FALSE).then_some(point)
}

fn primary_button_down() -> bool {
    let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
    is_key_down(if swapped { VK_RBUTTON } else { VK_LBUTTON })
}

pub(crate) fn capture(
    target: Target,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
    stopping: &Receiver<Infallible>,
    movie: Option<crate::MovieTarget>,
) {
    if let Err(failure) = record(target, interval, longest_edge, events, stopping, movie)
        && !stopping.is_closed()
    {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A closed lane means nobody is left to tell"
        )]
        let _ = events.force_send(failure.into());
    }
}

fn record(
    target: Target,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
    stopping: &Receiver<Infallible>,
    movie: Option<crate::MovieTarget>,
) -> Result<(), Failure> {
    join_lasting_apartment()?;
    let mut exclusions = Exclusions::default();
    exclusions.refresh()?;
    let mut clean_from = clean_after_now();
    let mut movie = movie::Movie::new(movie);
    let cadence = if movie.active() {
        interval.min(movie::INTERVAL)
    } else {
        interval
    };
    let mut capture = Capture::start(target, cadence, longest_edge, stopping)?;
    let mut pacer = Pacer::new(interval);
    let mut motion = Instant::now();
    let outcome = (|| {
        while !stopping.is_closed() && !events.is_closed() {
            if capture.poll(clean_from)? && exclusions.refresh()? {
                clean_from = clean_after_now();
                capture.discard_pending();
            }
            let now = Instant::now();
            let mut movie_failed = false;

            let still = pacer.due(now) && !events.is_full();
            let moving = movie.active() && now >= motion;
            if (still || moving)
                && let Some(frame) = capture.read_pending()?
            {
                movie_failed = !movie.frame(&frame, stopping);
                if moving {
                    motion = next_motion(motion, now);
                }
                if still && events.try_send(ScreenEvent::Frame(frame)).is_ok() {
                    pacer.sent(now);
                }
            }
            movie_failed |= !movie.poll(Instant::now(), stopping);
            if movie_failed {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A closed event lane means the owner has left; otherwise failure replaces the oldest queued still"
                )]
                let _ = events.force_send(ScreenEvent::MovieStopped);
            }
            thread::sleep(POLL);
        }
        Ok(())
    })();
    movie.stop(Instant::now());
    outcome
}

fn next_motion(motion: Instant, now: Instant) -> Instant {
    let elapsed = now
        .saturating_duration_since(motion)
        .as_nanos()
        .checked_div(movie::INTERVAL.as_nanos())
        .unwrap_or_default();
    let intervals = u32::try_from(elapsed).unwrap_or(u32::MAX).saturating_add(1);
    motion
        .checked_add(movie::INTERVAL.saturating_mul(intervals))
        .unwrap_or(now)
}

fn clean_after_now() -> Instant {
    let now = Instant::now();
    now.checked_add(COMPOSITION_MARGIN).unwrap_or(now)
}

struct Pacer {
    interval: Duration,
    last: Option<Instant>,
}

impl Pacer {
    const fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    fn due(&self, now: Instant) -> bool {
        self.last
            .is_none_or(|last| now.saturating_duration_since(last) >= self.interval)
    }

    const fn sent(&mut self, at: Instant) {
        self.last = Some(at);
    }
}

struct Capture {
    display: screen::Display,
    followed: Option<Window>,
    part: Part,
    content: [u32; 2],
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    item_closed: Receiver<Infallible>,
    closed_handler: i64,
    longest_edge: u32,
    readback: Readback,
    pending: Option<PendingFrame>,
}

struct PendingFrame {
    frame: Direct3D11CaptureFrame,
    at: Instant,
    part: Part,
}

impl Capture {
    fn start(
        target: Target,
        interval: Duration,
        longest_edge: u32,
        stopping: &Receiver<Infallible>,
    ) -> Result<Self, Failure> {
        let display = target.display().native;

        let access =
            GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless);
        let monitor = display.handle();
        let (device, context) = device_for(monitor)?;
        let dxgi: IDXGIDevice = device.cast()?;

        let direct3d: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;

        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor)? };
        let (closed, item_closed) = async_channel::bounded::<Infallible>(1);
        let closed_handler = item.Closed(&TypedEventHandler::new(move |_, _| {
            closed.close();
            Ok(())
        }))?;
        let size = item.Size()?;
        let pool =
            Direct3D11CaptureFramePool::CreateFreeThreaded(&direct3d, POOL_FORMAT, BUFFERS, size)?;
        let session = pool.CreateCaptureSession(&item)?;

        best_effort(session.SetIsCursorCaptureEnabled(false));

        best_effort(session.SetMinUpdateInterval(time_span(interval)));
        let content_size = pixels(size);
        let part = target.part();
        let crop = pixel_box(part, content_size);
        let readback = Readback::new(&device, crop_size(crop), longest_edge)?;
        session.StartCapture()?;
        if let Ok(access) = &access {
            settle(access, stopping);
        }

        best_effort(session.SetIsBorderRequired(false));
        let followed = match target.region {
            Region::Window(window, _) => Some(window),
            Region::Whole | Region::Area(_) => None,
        };
        Ok(Self {
            display: target.display,
            followed,
            part,
            content: content_size,
            device,
            context,
            item,
            pool,
            session,
            item_closed,
            closed_handler,
            longest_edge,
            readback,
            pending: None,
        })
    }

    fn poll(&mut self, clean_from: Instant) -> Result<bool, Failure> {
        if self.item_closed.is_closed() {
            return Err(Failure::DisplayLost);
        }
        if !is_current(self.display) {
            return Err(Failure::DisplayChanged);
        }

        if unsafe { self.device.GetDeviceRemovedReason() }.is_err() {
            return Err(Failure::DeviceLost);
        }
        let mut newest = None;
        while let Some(frame) = self.next_frame()? {
            newest = Some(frame);
        }
        let Some(frame) = newest else {
            return Ok(false);
        };
        let size = frame.ContentSize()?;
        let content = pixels(size);
        if content != self.content {
            self.pending = None;
            return Err(Failure::DisplayChanged);
        }
        let captured = composed_at(frame.SystemRelativeTime()?);
        if captured < clean_from {
            return Ok(false);
        }
        let part = self.follow()?;
        self.pending = Some(PendingFrame {
            frame,
            at: captured,
            part,
        });
        Ok(true)
    }

    fn next_frame(&self) -> Result<Option<Direct3D11CaptureFrame>, Failure> {
        match self.pool.TryGetNextFrame() {
            Ok(frame) => Ok(Some(frame)),
            Err(error) if error.code().is_ok() => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn discard_pending(&mut self) {
        self.pending = None;
    }

    fn read_pending(&mut self) -> Result<Option<Frame>, Failure> {
        let Some(PendingFrame { frame, at, part }) = self.pending.take() else {
            return Ok(None);
        };
        if self.follow()? != part {
            return Ok(None);
        }
        let surface: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;

        let texture: ID3D11Texture2D = unsafe { surface.GetInterface()? };
        let crop = pixel_box(part, self.content);
        let source = crop_size(crop);
        if source != self.readback.source() {
            self.readback = Readback::new(&self.device, source, self.longest_edge)?;
        }
        let [left, top, ..] = crop;
        let bgra = self.readback.read(&self.context, &texture, [left, top])?;
        if self.follow()? != part {
            return Ok(None);
        }
        let [width, height] = self.readback.size();
        Ok(Some(Frame {
            at,
            width,
            height,
            bgra,
            part: part_of_box(crop, self.content),
        }))
    }

    fn follow(&mut self) -> Result<Part, WindowGone> {
        if let Some(window) = self.followed {
            self.part = window_part(self.display.native, window)?.ok_or(WindowGone::Closed)?;
        }
        Ok(self.part)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        best_effort(self.item.RemoveClosed(self.closed_handler));

        best_effort(self.session.Close());
        best_effort(self.pool.Close());
    }
}

const fn crop_size([left, top, right, bottom]: [u32; 4]) -> [u32; 2] {
    [right.saturating_sub(left), bottom.saturating_sub(top)]
}

fn pixels(size: SizeInt32) -> [u32; 2] {
    [size.Width, size.Height].map(|edge| u32::try_from(edge).unwrap_or_default())
}

fn time_span(duration: Duration) -> TimeSpan {
    TimeSpan {
        Duration: i64::try_from(duration.as_nanos() / 100).unwrap_or(i64::MAX),
    }
}

fn composed_at(composed: TimeSpan) -> Instant {
    let now = Instant::now();
    qpc_ticks()
        .and_then(|ticks| now.checked_sub(age(ticks, composed.Duration)))
        .unwrap_or(now)
}

fn qpc_ticks() -> Option<i64> {
    let (mut count, mut frequency) = (0, 0);

    unsafe {
        QueryPerformanceCounter(&raw mut count);
        QueryPerformanceFrequency(&raw mut frequency);
    }
    let ticks = i128::from(count)
        .checked_mul(TICKS_PER_SECOND)?
        .checked_div(i128::from(frequency))?;
    i64::try_from(ticks).ok()
}

fn age(now: i64, then: i64) -> Duration {
    u64::try_from(now.saturating_sub(then)).map_or(Duration::ZERO, |ticks| {
        Duration::from_nanos(ticks.saturating_mul(100))
    })
}

fn settle(request: &IAsyncOperation<AppCapabilityAccessStatus>, stopping: &Receiver<Infallible>) {
    let Some(deadline) = Instant::now().checked_add(ACCESS_WAIT) else {
        return;
    };
    while matches!(request.Status(), Ok(AsyncStatus::Started))
        && !stopping.is_closed()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(1));
    }
}

fn best_effort<T>(result: windows::core::Result<T>) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Optional capture settings and teardown steps cannot change the outcome"
    )]
    let _ = result;
}

fn join_lasting_apartment() -> windows::core::Result<()> {
    let mut cookie = null_mut();

    HRESULT(unsafe { CoIncrementMTAUsage(&raw mut cookie) }).ok()
}

#[derive(Debug)]
enum Failure {
    Window(WindowGone),
    Exposed,
    DisplayLost,
    DisplayChanged,
    DeviceLost,
    Refused(windows::core::Error),
}

impl From<windows::core::Error> for Failure {
    fn from(error: windows::core::Error) -> Self {
        match error.code() {
            DXGI_ERROR_DEVICE_REMOVED
            | DXGI_ERROR_DEVICE_RESET
            | DXGI_ERROR_DEVICE_HUNG
            | DXGI_ERROR_DRIVER_INTERNAL_ERROR => Self::DeviceLost,
            _ => Self::Refused(error),
        }
    }
}

impl From<WindowGone> for Failure {
    fn from(gone: WindowGone) -> Self {
        Self::Window(gone)
    }
}

impl From<Failure> for ScreenEvent {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Window(gone) => Self::WindowGone(gone),
            failure => Self::Failed(failure.to_string()),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Window(gone) => gone.fmt(f),
            Self::Exposed => f.write_str(
                "Cannot keep Speakeasy's windows out of the Show. Start the Show again.",
            ),
            Self::DisplayLost => {
                f.write_str("The recorded display was disconnected. Start the Show again.")
            },
            Self::DisplayChanged => f.write_str(
                "The recorded display changed size, position, or scale. Start the Show again.",
            ),
            Self::DeviceLost => f.write_str("The graphics driver restarted. Start the Show again."),
            Self::Refused(error) => write!(
                f,
                "Windows stopped the screen capture ({}). Start the Show again.",
                error.code()
            ),
        }
    }
}

impl error::Error for Failure {}

pub(crate) fn configure_overlay(window: &NativeWindow, display: Display) -> anyhow::Result<()> {
    let hwnd = window.native.hwnd;

    exclude_always(hwnd)?;

    window.configure_pill()?;
    let [left, top, ..] = display.edges;
    let [width, height] = display.extent();
    let place = || {
        ok_or_last_error(unsafe {
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                left,
                top,
                width,
                height,
                SWP_NOACTIVATE | SWP_FRAMECHANGED,
            )
        })
    };

    let dpi = || unsafe { GetDpiForWindow(hwnd) };
    let before = dpi();
    place()?;

    if dpi() != before {
        place()?;
    }
    Ok(())
}

pub(crate) fn set_overlay_input(window: &NativeWindow, takes_input: bool) {
    window.native.restyle(GWL_EXSTYLE, |style| {
        if takes_input {
            style & !WS_EX_TRANSPARENT
        } else {
            style | WS_EX_TRANSPARENT
        }
    });
}

pub(crate) fn set_overlay_visible(window: &NativeWindow, visible: bool) {
    let hwnd = window.native.hwnd;

    unsafe {
        ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        if visible {
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

#[cfg(test)]
mod acceptance;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_positions_normalize_to_their_display() {
        let display = Display {
            monitor: 0,
            edges: [-3840, -794, 0, 1366],
        };
        let at = |x: i32, y: i32| display.bounds().normalize([x, y].map(f64::from));
        assert_eq!(at(-1920, 286), Some([0.5, 0.5]));
        assert_eq!(
            at(960, 1366),
            Some([1.25, 1.0]),
            "A pointer on the next display must not clamp onto this one's edge"
        );
        let desktop = Display {
            monitor: 0,
            edges: [i32::MIN, i32::MIN, i32::MAX, i32::MAX],
        };
        assert_eq!(
            desktop
                .bounds()
                .normalize([i32::MAX, i32::MIN].map(f64::from)),
            Some([1.0, 0.0]),
            "Spans across the whole coordinate range must not overflow"
        );
    }

    #[test]
    fn frames_are_sent_at_most_once_per_interval() {
        let start = Instant::now();
        let at = |milliseconds| {
            start
                .checked_add(Duration::from_millis(milliseconds))
                .unwrap()
        };
        let mut pacer = Pacer::new(Duration::from_millis(200));
        assert!(pacer.due(at(0)), "The first frame waits for nothing");
        pacer.sent(at(0));
        assert!(!pacer.due(at(199)));
        assert!(pacer.due(at(200)));
        pacer.sent(at(250));
        assert!(!pacer.due(at(449)), "Spacing runs from the last frame sent");
        assert!(pacer.due(at(450)));
    }
}
