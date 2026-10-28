//! Screen capture, pointer sampling, and the Show overlay on Windows.
//!
//! Frames come from Windows.Graphics.Capture on a Direct3D 11 device of the adapter that drives the
//! display. The capture thread polls the frame pool itself, so no frame callback shares its state;
//! the one notice that arrives from another thread, the display going away, only closes a channel.
//! While a capture runs it leaves every Speakeasy window out of capture, and afterwards shows them
//! to capture again, so they still appear in the user's own screen sharing.
//!
//! A Show of part of the display reads only that part of each frame back, following a window's
//! frame bounds from frame to frame.
//!
//! Every position is in physical pixels: the manifest makes Speakeasy per-monitor DPI aware, so the
//! cursor, monitor bounds, and window placement share one coordinate space.

mod exclusion;
mod readback;

use std::{
    convert::Infallible,
    error, fmt,
    ptr::null_mut,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, ensure};
use async_channel::{Receiver, Sender};
use windows::{
    Foundation::{TimeSpan, TypedEventHandler},
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess,
            GraphicsCaptureAccessKind, GraphicsCaptureItem, GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
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
    Foundation::{FALSE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::{
        Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
        Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
    },
    System::{
        Com::CoIncrementMTAUsage,
        Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
    },
    UI::{
        HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
        Input::KeyboardAndMouse::{VK_LBUTTON, VK_RBUTTON},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GA_ROOT, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetCursorPos, GetSystemMetrics,
            GetWindowLongPtrW, HWND_TOPMOST, IsIconic, IsWindow, SM_SWAPBUTTON, SW_HIDE,
            SW_SHOWNOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_NCCALCSIZE, WM_NCDESTROY,
            WS_EX_TRANSPARENT, WindowFromPoint,
        },
    },
};

use self::{
    exclusion::{Exclusions, exclude_always, exclusion_supported, owned},
    readback::{Readback, device_for},
};
use super::{BASE_DPI, is_key_down, modifiers, ok_or_last_error};
use crate::{
    NativeWindow,
    screen::{
        self, Frame, Part, PointerSample, Region, ScreenEvent, Target,
        geometry::{Bounds, part_of_box, pixel_box},
    },
};

/// How often the capture thread drains the frame pool and checks for a stop.
const POLL: Duration = Duration::from_millis(10);
/// Pool buffers, each a full-resolution frame of GPU memory: one may wait for its turn to be sent
/// while Windows fills the other. Draining the pool replaces the waiting frame, so two never stall.
const BUFFERS: i32 = 2;
/// How long the request for border-free capture may hold back the first frame.
const ACCESS_WAIT: Duration = Duration::from_millis(250);
const POOL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
/// Windows Runtime time spans count 100 ns ticks.
const TICKS_PER_SECOND: i128 = 10_000_000;
/// A frame's timestamp may mark when it was presented rather than when its composition began, a
/// refresh later at most; frames stamped this soon after an exclusion may predate it.
const COMPOSITION_MARGIN: Duration = Duration::from_millis(50);
/// The window classes of the desktop and the taskbars, which a Show of a window passes over.
const SHELL_CLASSES: [&str; 4] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// A monitor and where it sits on the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Display {
    /// The `HMONITOR`, kept as an integer because handles are not `Send`.
    monitor: isize,
    /// Left, top, right, and bottom edges in physical pixels.
    edges: [i32; 4],
}

impl Display {
    fn handle(self) -> HMONITOR {
        HMONITOR(self.monitor as *mut _)
    }

    /// Width and height in physical pixels.
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

/// A rectangle of desktop pixels, measured in `f64` so a span across the whole coordinate range
/// cannot overflow.
impl From<RECT> for Bounds {
    fn from(rect: RECT) -> Self {
        let [left, top, right, bottom] = edges(rect).map(f64::from);
        Self {
            origin: [left, top],
            size: [right - left, bottom - top],
        }
    }
}

/// The left, top, right, and bottom edges of `rect`.
const fn edges(rect: RECT) -> [i32; 4] {
    [rect.left, rect.top, rect.right, rect.bottom]
}

/// A top-level window, kept as an integer because handles are not `Send`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window(isize);

impl Window {
    fn handle(self) -> HWND {
        self.0 as HWND
    }
}

/// The top-level window under the pointer and the part of `display` it covers, unless it is the
/// desktop, a taskbar, or Speakeasy's own.
pub(crate) fn window_under_pointer(display: Display) -> Option<(Window, Part)> {
    let window = top_level_at(cursor()?);
    if window.is_null() || owned(window) || SHELL_CLASSES.contains(&class_name(window).as_str()) {
        return None;
    }
    let window = Window(window as isize);
    Some((window, window_part(display, window)?))
}

/// The top-level window at `point` on the desktop; null where there is none.
fn top_level_at(point: POINT) -> HWND {
    // SAFETY: both calls take values and answer null when no window qualifies.
    unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) }
}

/// The name of `window`'s class, empty once the window is gone.
fn class_name(window: HWND) -> String {
    let mut class = [0_u16; 256];
    // SAFETY: the buffer is a live local of the length given; a stale handle reports no class.
    let length = unsafe {
        GetClassNameW(
            window,
            class.as_mut_ptr(),
            i32::try_from(class.len()).unwrap_or_default(),
        )
    };
    String::from_utf16_lossy(
        class
            .get(..usize::try_from(length).unwrap_or_default())
            .unwrap_or_default(),
    )
}

/// The part of `display` that `window` covers now, by its visible frame without the resize
/// border; `None` once it is closed, minimized, or off the display.
fn window_part(display: Display, window: Window) -> Option<Part> {
    let window = window.handle();
    // SAFETY: both calls take the handle by value and tolerate a stale one.
    let placed = unsafe { IsWindow(window) != FALSE && IsIconic(window) == FALSE };
    if !placed {
        return None;
    }
    let frame = frame_bounds(window)?;
    display.bounds().part_covered_by(Bounds::from(frame))
}

/// The visible frame of `window` on the desktop, without its resize border; `None` once it is
/// gone.
fn frame_bounds(window: HWND) -> Option<RECT> {
    let mut frame = RECT::default();
    let size = u32::try_from(size_of::<RECT>()).ok()?;
    // SAFETY: the bounds land in a live local of the size given; a stale handle fails the call.
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
    let pointer = cursor().context("Cannot find the pointer")?;
    // SAFETY: takes the point by value and always answers with the nearest monitor.
    describe(unsafe { MonitorFromPoint(pointer, MONITOR_DEFAULTTONEAREST) })
        .context("Cannot measure the display under the pointer")
}

/// Where `monitor` sits on the desktop, and its size in logical pixels.
fn describe(
    monitor: windows_sys::Win32::Graphics::Gdi::HMONITOR,
) -> anyhow::Result<screen::Display> {
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>())?,
        ..Default::default()
    };
    // SAFETY: fills the live local, whose size field the call checks.
    ok_or_last_error(unsafe { GetMonitorInfoW(monitor, &raw mut info) })?;
    let (mut dpi, mut vertical_dpi) = (0, 0);
    // SAFETY: both out-parameters are live locals.
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

pub(crate) fn pointer(display: Display) -> Option<PointerSample> {
    let at = Instant::now();
    let POINT { x, y } = cursor()?;
    let [x, y] = display.bounds().normalize([x, y].map(f64::from))?;
    Some(PointerSample {
        at,
        x,
        y,
        drawing: modifiers().draws(),
        pressed: primary_button_down(),
    })
}

fn cursor() -> Option<POINT> {
    let mut point = POINT::default();
    // SAFETY: GetCursorPos writes only this live local.
    (unsafe { GetCursorPos(&raw mut point) } != FALSE).then_some(point)
}

/// `GetAsyncKeyState` reads physical buttons, so a swapped mouse's primary button is its right one.
fn primary_button_down() -> bool {
    // SAFETY: GetSystemMetrics takes an index by value.
    let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
    is_key_down(if swapped { VK_RBUTTON } else { VK_LBUTTON })
}

pub(crate) fn capture(
    target: Target,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
    stopping: &Receiver<Infallible>,
) {
    if let Err(failure) = record(target, interval, longest_edge, events, stopping)
        && !stopping.is_closed()
    {
        // A full lane gives up its oldest frame so that the failure always arrives.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A closed lane means nobody is left to tell"
        )]
        let _ = events.force_send(ScreenEvent::Failed(failure.to_string()));
    }
}

fn record(
    target: Target,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
    stopping: &Receiver<Infallible>,
) -> Result<(), Failure> {
    join_lasting_apartment()?;
    if !exclusion_supported() {
        return Err(Failure::OldWindows);
    }
    let mut exclusions = Exclusions::default();
    exclusions.refresh()?;
    let mut clean_from = clean_after_now();
    let mut capture = Capture::start(target, interval, longest_edge, stopping)?;
    let mut pacer = Pacer::new(interval);
    while !stopping.is_closed() && !events.is_closed() {
        // A window opened since the last check is in the first frame Windows composes with it.
        if capture.poll(clean_from)? && exclusions.refresh()? {
            clean_from = clean_after_now();
            capture.discard_pending();
        }
        let now = Instant::now();
        // A full lane leaves the pending frame waiting rather than reading back one to drop.
        if pacer.due(now)
            && !events.is_full()
            && let Some(frame) = capture.read_pending()?
            && events.try_send(ScreenEvent::Frame(frame)).is_ok()
        {
            pacer.sent(now);
        }
        thread::sleep(POLL);
    }
    Ok(())
}

/// The earliest frame timestamp that cannot predate an exclusion made now.
fn clean_after_now() -> Instant {
    let now = Instant::now();
    now.checked_add(COMPOSITION_MARGIN).unwrap_or(now)
}

/// Spaces sent frames at least `interval` apart.
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

/// A capture of one display, all of it or a part, polled on the capture thread.
struct Capture {
    display: Display,
    /// The window whose place frames follow, when recording one.
    followed: Option<Window>,
    /// The part of the display frames show: fixed for an area, following a window.
    part: Part,
    /// The size Windows composes the display at.
    content: [u32; 2],
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// The same device as the Windows Runtime sees it, for recreating the pool.
    direct3d: IDirect3DDevice,
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    /// Closes once Windows closes the item, when the display goes away.
    item_closed: Receiver<Infallible>,
    closed_handler: i64,
    longest_edge: u32,
    /// Reads back the part of each frame shown, at that part's size.
    readback: Readback,
    /// The newest frame not yet read, and when it was on screen.
    pending: Option<(Direct3D11CaptureFrame, Instant)>,
}

impl Capture {
    fn start(
        target: Target,
        interval: Duration,
        longest_edge: u32,
        stopping: &Receiver<Infallible>,
    ) -> Result<Self, Failure> {
        let display = target.display().native;
        // Asked first, so Windows answers while the device is made.
        let access =
            GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless);
        let monitor = display.handle();
        let (device, context) = device_for(monitor)?;
        let dxgi: IDXGIDevice = device.cast()?;
        // SAFETY: wraps the live DXGI device; the runtime keeps its own reference.
        let direct3d: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? }.cast()?;
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        // SAFETY: the monitor handle came from the system; a stale one fails the call.
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
        // The agent sees the pointer as click marks, not as a cursor in every frame.
        best_effort(session.SetIsCursorCaptureEnabled(false));
        // Builds without the setting deliver every change; `record` paces those itself.
        best_effort(session.SetMinUpdateInterval(time_span(interval)));
        let composed = pixels(size);
        let part = target.part();
        let readback = Readback::new(&device, crop_size(pixel_box(part, composed)), longest_edge)?;
        session.StartCapture()?;
        if let Ok(access) = &access {
            settle(access, stopping);
        }
        // Windows refuses this before capture starts, even with access granted.
        best_effort(session.SetIsBorderRequired(false));
        let followed = match target.region {
            Region::Window(window, _) => Some(window),
            Region::Whole | Region::Area(_) => None,
        };
        Ok(Self {
            display,
            followed,
            part,
            content: composed,
            device,
            context,
            direct3d,
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

    /// Takes the newest frame Windows composed since `clean_from` as the pending one, reporting
    /// whether there was one; fails once the display or the graphics device is gone.
    fn poll(&mut self, clean_from: Instant) -> Result<bool, Failure> {
        if self.item_closed.is_closed() {
            return Err(Failure::DisplayLost);
        }
        // SAFETY: asks a live device whether it was removed.
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
            // The display changed resolution or rotation: this frame fills only part of its
            // surface, and the pending one belongs to the pool being replaced.
            self.pending = None;
            self.pool
                .Recreate(&self.direct3d, POOL_FORMAT, BUFFERS, size)?;
            self.content = content;
            return Ok(false);
        }
        let captured = composed_at(frame.SystemRelativeTime()?);
        if captured < clean_from {
            return Ok(false);
        }
        self.pending = Some((frame, captured));
        Ok(true)
    }

    fn next_frame(&self) -> Result<Option<Direct3D11CaptureFrame>, Failure> {
        match self.pool.TryGetNextFrame() {
            Ok(frame) => Ok(Some(frame)),
            // An empty pool answers with a null frame, which surfaces as an error without a code.
            Err(error) if error.code().is_ok() => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn discard_pending(&mut self) {
        self.pending = None;
    }

    /// Reads the pending frame back to memory, leaving none pending.
    fn read_pending(&mut self) -> Result<Option<Frame>, Failure> {
        let Some((frame, captured)) = self.pending.take() else {
            return Ok(None);
        };
        let surface: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        // SAFETY: the frame's surface is a live texture on this capture's device.
        let texture: ID3D11Texture2D = unsafe { surface.GetInterface()? };
        let crop = pixel_box(self.follow(), self.content);
        let source = crop_size(crop);
        if source != self.readback.source {
            self.readback = Readback::new(&self.device, source, self.longest_edge)?;
        }
        let [left, top, ..] = crop;
        let bgra = self.readback.read(&self.context, &texture, [left, top])?;
        let [width, height] = self.readback.size;
        Ok(Some(Frame {
            captured,
            width,
            height,
            bgra,
            part: part_of_box(crop, self.content),
        }))
    }

    /// The part of the display to read: a window's place now, or its last place while it is
    /// minimized, closed, or off the display.
    fn follow(&mut self) -> Part {
        if let Some(window) = self.followed
            && let Some(part) = window_part(self.display, window)
        {
            self.part = part;
        }
        self.part
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        best_effort(self.item.RemoveClosed(self.closed_handler));
        // Closing stops capture now rather than whenever Windows releases its last reference.
        best_effort(self.session.Close());
        best_effort(self.pool.Close());
    }
}

/// The width and height of a box of pixels given by its edges.
const fn crop_size([left, top, right, bottom]: [u32; 4]) -> [u32; 2] {
    [right.saturating_sub(left), bottom.saturating_sub(top)]
}

fn pixels(size: windows::Graphics::SizeInt32) -> [u32; 2] {
    // A negative size cannot occur; zero makes texture creation report the problem.
    [size.Width, size.Height].map(|edge| u32::try_from(edge).unwrap_or_default())
}

fn time_span(duration: Duration) -> TimeSpan {
    TimeSpan {
        Duration: i64::try_from(duration.as_nanos() / 100).unwrap_or(i64::MAX),
    }
}

/// When a frame Windows composed at `composed` was on screen. Windows stamps frames on the QPC
/// clock that `Instant` reads, but an `Instant` cannot be built from a QPC reading, so the frame's
/// age is measured instead.
fn composed_at(composed: TimeSpan) -> Instant {
    let now = Instant::now();
    qpc_ticks()
        .and_then(|ticks| now.checked_sub(age(ticks, composed.Duration)))
        .unwrap_or(now)
}

/// The QPC clock in the 100 ns ticks of Windows Runtime time spans.
fn qpc_ticks() -> Option<i64> {
    let (mut count, mut frequency) = (0, 0);
    // SAFETY: each call writes only its live local; neither has failed since Windows XP.
    unsafe {
        QueryPerformanceCounter(&raw mut count);
        QueryPerformanceFrequency(&raw mut frequency);
    }
    let ticks = i128::from(count)
        .checked_mul(TICKS_PER_SECOND)?
        .checked_div(i128::from(frequency))?;
    i64::try_from(ticks).ok()
}

/// How long before `now` something stamped `then` happened, both in 100 ns ticks; a stamp from
/// the future happened now.
fn age(now: i64, then: i64) -> Duration {
    u64::try_from(now.saturating_sub(then)).map_or(Duration::ZERO, |ticks| {
        Duration::from_nanos(ticks.saturating_mul(100))
    })
}

/// Waits briefly for Windows to answer an access request; a stop or a slow answer ends the wait.
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

/// Capture works without the settings a Windows build may lack, and is already over when a
/// teardown step fails.
fn best_effort<T>(result: windows::core::Result<T>) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Optional capture settings and teardown steps cannot change the outcome"
    )]
    let _ = result;
}

/// Makes the capture thread a member of a multithreaded apartment that lasts as long as the
/// process. The `windows` crate caches activation factories process-wide, and they would dangle
/// once the last apartment closed, so a capture after the first would crash.
fn join_lasting_apartment() -> windows::core::Result<()> {
    let mut cookie = null_mut();
    // SAFETY: writes only the live local. The cookie is never handed back, by design.
    HRESULT(unsafe { CoIncrementMTAUsage(&raw mut cookie) }).ok()
}

/// Why a capture ended early, worded for the person recording.
#[derive(Debug)]
enum Failure {
    /// Windows predates leaving windows out of capture.
    OldWindows,
    /// A Speakeasy window could not be left out of capture.
    Exposed,
    /// The display was disconnected or turned off.
    DisplayLost,
    /// The graphics driver restarted or the adapter went away.
    DeviceLost,
    /// Windows refused a capture step.
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

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OldWindows => formatter.write_str("Shows need Windows 10 version 2004 or later."),
            Self::Exposed => formatter.write_str(
                "Windows would not keep Speakeasy out of the recording. Start the Show again.",
            ),
            Self::DisplayLost => {
                formatter.write_str("The recorded display was disconnected. Start the Show again.")
            },
            Self::DeviceLost => {
                formatter.write_str("The graphics driver restarted. Start the Show again.")
            },
            Self::Refused(error) => write!(
                formatter,
                "Windows stopped the screen capture ({}). Start the Show again.",
                error.code()
            ),
        }
    }
}

impl error::Error for Failure {}

pub(crate) fn configure_overlay(window: &NativeWindow, display: Display) -> anyhow::Result<()> {
    let hwnd = window.native.hwnd;
    // First, so a build that cannot exclude the overlay leaves the window untouched.
    exclude_always(hwnd)?;
    // The overlay is the pill's click-through popup, stretched over the display.
    window.configure_pill()?;
    // SAFETY: this thread owns the window, which owns the subclass until WM_NCDESTROY on this same
    // thread; installing it again replaces it.
    ok_or_last_error(unsafe { SetWindowSubclass(hwnd, Some(edge_to_edge), 1, 0) })?;
    let [left, top, ..] = display.edges;
    let [width, height] = display.extent();
    let place = || {
        // SAFETY: this thread owns the window. A changed frame recomputes the client area through
        // `edge_to_edge` even when the window does not move.
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
    // SAFETY: reads the DPI of this thread's live window by handle.
    let dpi = || unsafe { GetDpiForWindow(hwnd) };
    let before = dpi();
    place()?;
    // Arriving on a display of another DPI makes GPUI resize the window to the size Windows
    // suggests; placing it again from that display lands it on the exact bounds.
    if dpi() != before {
        place()?;
    }
    Ok(())
}

/// Makes the overlay's whole window its client area. GPUI keeps a resize border's width of every
/// window without a title bar out of its client area, which would leave the display's edges
/// uncovered and draw ink up to a border's width from the pointer.
unsafe extern "system" fn edge_to_edge(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    // SAFETY: this window-owned subclass runs on the window's thread and is removed at
    // destruction.
    unsafe {
        match message {
            // Answering zero keeps the proposed window rectangle as the client area.
            WM_NCCALCSIZE if wparam != 0 => 0,
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(edge_to_edge), id);
                DefSubclassProc(hwnd, message, wparam, lparam)
            },
            _ => DefSubclassProc(hwnd, message, wparam, lparam),
        }
    }
}

pub(crate) fn set_overlay_input(window: &NativeWindow, takes_input: bool) {
    let hwnd = window.native.hwnd;
    let transparent = WS_EX_TRANSPARENT.cast_signed() as isize;
    // SAFETY: this thread owns the window. Without WS_EX_TRANSPARENT the layered overlay takes
    // every click over it; WS_EX_NOACTIVATE still keeps those clicks from activating it.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let style = if takes_input {
            style & !transparent
        } else {
            style | transparent
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
    }
}

pub(crate) fn set_overlay_visible(window: &NativeWindow, visible: bool) {
    let hwnd = window.native.hwnd;
    // SAFETY: this thread owns the window.
    unsafe {
        ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        if visible {
            // Topmost windows shown since stack above it; return it to the top of the band.
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
        // A display left of and above the primary one, as desktop coordinates allow.
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
