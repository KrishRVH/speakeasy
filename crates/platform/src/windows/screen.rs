//! Screen capture, pointer sampling, and the Show overlay on Windows.
//!
//! Frames come from Windows.Graphics.Capture on a Direct3D 11 device of the adapter that drives the
//! display. The capture thread polls the frame pool itself, so no frame callback shares its state;
//! the one notice that arrives from another thread, the display going away, only closes a channel.
//! While a capture runs it leaves every Speakeasy window out of capture, and afterwards shows them
//! to capture again, so they still appear in the user's own screen sharing.
//!
//! Every position is in physical pixels: the manifest makes Speakeasy per-monitor DPI aware, so the
//! cursor, monitor bounds, and window placement share one coordinate space.

use std::{
    convert::Infallible,
    error, fmt,
    ptr::null_mut,
    slice, thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, ensure};
use async_channel::{Receiver, Sender};
use raw_window_handle::RawWindowHandle;
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
        Foundation::{E_UNEXPECTED, HMODULE},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN},
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
                D3D11_MAPPED_SUBRESOURCE, D3D11_RESOURCE_MISC_GENERATE_MIPS, D3D11_SDK_VERSION,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
                ID3D11DeviceContext, ID3D11ShaderResourceView, ID3D11Texture2D,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                CreateDXGIFactory1, DXGI_ERROR_DEVICE_HUNG, DXGI_ERROR_DEVICE_REMOVED,
                DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_DRIVER_INTERNAL_ERROR, IDXGIDevice,
                IDXGIFactory1,
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
use windows_sys::{
    Wdk::System::SystemServices::RtlGetVersion,
    Win32::{
        Foundation::{FALSE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM},
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
        System::{
            Com::CoIncrementMTAUsage,
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            SystemInformation::OSVERSIONINFOW,
            Threading::GetCurrentProcessId,
        },
        UI::{
            HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{
                VK_CONTROL, VK_LBUTTON, VK_LWIN, VK_MENU, VK_RBUTTON, VK_RWIN, VK_SHIFT,
            },
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                EnumWindows, GetCursorPos, GetPropW, GetSystemMetrics, GetWindowDisplayAffinity,
                GetWindowThreadProcessId, HWND_TOPMOST, IsWindowVisible, RemovePropW,
                SM_SWAPBUTTON, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
                SetPropW, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
                WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WM_NCDESTROY,
            },
        },
    },
    core::{BOOL, PCWSTR},
    w,
};

use super::{BASE_DPI, configure_pill, hwnd_of, is_key_down, ok_or_last_error};
use crate::screen::{
    Frame, PointerSample, ScreenEvent,
    geometry::{Bounds, fit, pack_rows},
};

/// How often the capture thread drains the frame pool and checks for a stop.
const POLL: Duration = Duration::from_millis(10);
/// Pool buffers, each a full-resolution frame of GPU memory: one may wait for its turn to be sent
/// while Windows fills the other. Draining the pool replaces the waiting frame, so two never stall.
const BUFFERS: i32 = 2;
/// How long the request for border-free capture may hold back the first frame.
const ACCESS_WAIT: Duration = Duration::from_millis(250);
const POOL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const BYTES_PER_PIXEL: usize = 4;
/// Windows Runtime time spans count 100 ns ticks.
const TICKS_PER_SECOND: i128 = 10_000_000;
/// The first build that leaves excluded windows out of capture; earlier builds black them out.
const EXCLUSION_BUILD: u32 = 19_041;
/// A frame's timestamp may mark when it was presented rather than when its composition began, a
/// refresh later at most; frames stamped this soon after an exclusion may predate it.
const COMPOSITION_MARGIN: Duration = Duration::from_millis(50);
/// Marks a window `configure_overlay` left out of all screen capture, so that a capture ending
/// never shows it to capture again.
const ALWAYS_EXCLUDED: PCWSTR = w!("SpeakeasyAlwaysExcluded");

/// A monitor and where it sits on the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Display {
    /// The `HMONITOR`, kept as an integer because handles are not `Send`.
    monitor: isize,
    /// Left, top, right, and bottom edges in physical pixels.
    bounds: [i32; 4],
}

impl Display {
    fn handle(self) -> HMONITOR {
        HMONITOR(self.monitor as *mut _)
    }

    /// Width and height in physical pixels.
    fn extent(self) -> [i32; 2] {
        let [left, top, right, bottom] = self.bounds;
        [right.saturating_sub(left), bottom.saturating_sub(top)]
    }

    fn area(self) -> Bounds {
        let [left, top, right, bottom] = self.bounds.map(f64::from);
        Bounds {
            origin: [left, top],
            size: [right - left, bottom - top],
        }
    }
}

pub(crate) fn display_under_pointer() -> anyhow::Result<crate::screen::Display> {
    let pointer = cursor().context("Cannot find the pointer")?;
    // SAFETY: takes the point by value and always answers with the nearest monitor.
    describe(unsafe { MonitorFromPoint(pointer, MONITOR_DEFAULTTONEAREST) })
        .context("Cannot measure the display under the pointer")
}

/// Where `monitor` sits on the desktop, and its size in logical pixels.
fn describe(
    monitor: windows_sys::Win32::Graphics::Gdi::HMONITOR,
) -> anyhow::Result<crate::screen::Display> {
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>())?,
        ..Default::default()
    };
    let (mut dpi, mut vertical_dpi) = (0, 0);
    // SAFETY: the out-parameters are live locals of the sizes these calls expect.
    let (measured, scaled) = unsafe {
        (
            GetMonitorInfoW(monitor, &raw mut info),
            GetDpiForMonitor(
                monitor,
                MDT_EFFECTIVE_DPI,
                &raw mut dpi,
                &raw mut vertical_dpi,
            ),
        )
    };
    ok_or_last_error(measured)?;
    ensure!(scaled >= 0 && dpi > 0, "Cannot read the display's scale");
    let RECT {
        left,
        top,
        right,
        bottom,
    } = info.rcMonitor;
    let native = Display {
        monitor: monitor as isize,
        bounds: [left, top, right, bottom],
    };
    let scale = dpi as f32 / BASE_DPI as f32;
    Ok(crate::screen::Display {
        native,
        size: native.extent().map(|edge| edge as f32 / scale),
    })
}

pub(crate) fn pointer(display: Display) -> Option<PointerSample> {
    let at = Instant::now();
    let POINT { x, y } = cursor()?;
    let [x, y] = display.area().normalize([x, y].map(f64::from))?;
    Some(PointerSample {
        at,
        x,
        y,
        drawing: shift_alone(),
        pressed: primary_button_down(),
    })
}

fn cursor() -> Option<POINT> {
    let mut point = POINT::default();
    // SAFETY: GetCursorPos writes only this live local.
    (unsafe { GetCursorPos(&raw mut point) } != FALSE).then_some(point)
}

/// Shift held without Ctrl, Alt, or Windows, so Shift shortcuts never draw.
fn shift_alone() -> bool {
    is_key_down(VK_SHIFT)
        && ![VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .into_iter()
            .any(is_key_down)
}

/// `GetAsyncKeyState` reads physical buttons, so a swapped mouse's primary button is its right one.
fn primary_button_down() -> bool {
    // SAFETY: GetSystemMetrics takes an index by value.
    let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
    is_key_down(if swapped { VK_RBUTTON } else { VK_LBUTTON })
}

pub(crate) fn capture(
    display: crate::screen::Display,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
    stopping: &Receiver<Infallible>,
) {
    if let Err(failure) = record(display.native, interval, longest_edge, events, stopping)
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
    display: Display,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
    stopping: &Receiver<Infallible>,
) -> Result<(), Failure> {
    join_lasting_apartment()?;
    if windows_build().is_none_or(|build| build < EXCLUSION_BUILD) {
        return Err(Failure::OldWindows);
    }
    let mut exclusions = Exclusions::default();
    exclusions.refresh()?;
    // Frames composed before a Speakeasy window was left out may show it.
    let mut clean_from = clean_after_now();
    let mut capture = Capture::start(display, interval, longest_edge, stopping)?;
    let mut pacer = Pacer::new(interval);
    while !stopping.is_closed() && !events.is_closed() {
        // A window opened since the last check is in the first frame Windows composes with it.
        if capture.poll(clean_from)? && exclusions.refresh()? {
            clean_from = clean_after_now();
            capture.forget();
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

/// The Speakeasy windows one capture has left out, shown to capture again when it ends.
#[derive(Default)]
struct Exclusions {
    changed: Vec<HWND>,
}

impl Exclusions {
    /// Leaves out every window of this process that capture would still show, hidden ones too so
    /// they stay out once shown. Reports whether any was newly left out: even one hidden now may
    /// have been shown in frames composed since the last check.
    fn refresh(&mut self) -> Result<bool, Failure> {
        let mut newcomers = false;
        let own = top_level_windows()?
            .into_iter()
            .filter(|&window| owned(window));
        for window in own.filter(|&window| affinity(window) == Some(WDA_NONE)) {
            // SAFETY: both calls take a window of this process by value.
            let (excluded, visible) = unsafe {
                (
                    SetWindowDisplayAffinity(window, WDA_EXCLUDEFROMCAPTURE) != FALSE,
                    IsWindowVisible(window) != FALSE,
                )
            };
            if excluded {
                self.changed.push(window);
                newcomers = true;
            } else if visible {
                return Err(Failure::Exposed);
            }
        }
        Ok(newcomers)
    }
}

impl Drop for Exclusions {
    fn drop(&mut self) {
        let restorable = |window: HWND| {
            // SAFETY: reads a property by handle; a destroyed window has none.
            let marked = !unsafe { GetPropW(window, ALWAYS_EXCLUDED) }.is_null();
            !marked && affinity(window) == Some(WDA_EXCLUDEFROMCAPTURE)
        };
        for &window in self.changed.iter().filter(|&&window| restorable(window)) {
            // SAFETY: takes values only; a window since destroyed, or reused by another process,
            // fails the call.
            unsafe { SetWindowDisplayAffinity(window, WDA_NONE) };
        }
    }
}

/// Every top-level window on the desktop, front to back.
fn top_level_windows() -> windows::core::Result<Vec<HWND>> {
    unsafe extern "system" fn collect(window: HWND, windows: LPARAM) -> BOOL {
        // SAFETY: `top_level_windows` passes its live vector for the length of the enumeration.
        unsafe { (*(windows as *mut Vec<HWND>)).push(window) };
        TRUE
    }
    let mut windows = Vec::new();
    // SAFETY: the callback only pushes into the vector, which outlives the enumeration.
    let enumerated = unsafe { EnumWindows(Some(collect), (&raw mut windows) as LPARAM) };
    if enumerated == FALSE {
        return Err(windows::core::Error::from_thread());
    }
    Ok(windows)
}

fn owned(window: HWND) -> bool {
    let mut process = 0;
    // SAFETY: writes only the live local; neither call retains anything.
    unsafe {
        GetWindowThreadProcessId(window, &raw mut process);
        process == GetCurrentProcessId()
    }
}

fn affinity(window: HWND) -> Option<u32> {
    let mut affinity = 0;
    // SAFETY: writes only the live local.
    (unsafe { GetWindowDisplayAffinity(window, &raw mut affinity) } != FALSE).then_some(affinity)
}

/// A capture of one display, polled on the capture thread.
struct Capture {
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
    /// Reads frames of the size Windows delivers.
    readback: Readback,
    /// The newest frame not yet read, and when it was on screen.
    pending: Option<(Direct3D11CaptureFrame, Instant)>,
}

impl Capture {
    fn start(
        display: Display,
        interval: Duration,
        longest_edge: u32,
        stopping: &Receiver<Infallible>,
    ) -> Result<Self, Failure> {
        // Asked first, so Windows answers while the device is made.
        let access =
            GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless);
        let monitor = display.handle();
        let (device, context) = device_for(monitor)?;
        // SAFETY: wraps the live DXGI device; the runtime keeps its own reference.
        let direct3d: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&device.cast::<IDXGIDevice>()?)? }
                .cast()?;
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
        let readback = Readback::new(&device, pixels(size), longest_edge)?;
        session.StartCapture()?;
        if let Ok(access) = &access {
            settle(access, stopping);
        }
        // Windows refuses this before capture starts, even with access granted.
        best_effort(session.SetIsBorderRequired(false));
        Ok(Self {
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
        let content = frame.ContentSize()?;
        if pixels(content) != self.readback.source {
            // The display changed resolution or rotation: this frame fills only part of its
            // surface, and the pending one belongs to the pool being replaced.
            self.pending = None;
            self.pool
                .Recreate(&self.direct3d, POOL_FORMAT, BUFFERS, content)?;
            self.readback = Readback::new(&self.device, pixels(content), self.longest_edge)?;
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

    fn forget(&mut self) {
        self.pending = None;
    }

    /// Reads the pending frame back to memory, leaving none pending.
    fn read_pending(&mut self) -> Result<Option<Frame>, Failure> {
        let Some((frame, captured)) = self.pending.take() else {
            return Ok(None);
        };
        // SAFETY: the frame's surface is a live texture on this capture's device.
        let texture: ID3D11Texture2D = unsafe {
            frame
                .Surface()?
                .cast::<IDirect3DDxgiInterfaceAccess>()?
                .GetInterface()?
        };
        let bgra = self.readback.read(&self.context, &texture)?;
        let [width, height] = self.readback.size;
        Ok(Some(Frame {
            captured,
            width,
            height,
            bgra,
        }))
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

/// A Direct3D 11 device on the adapter that drives `monitor`, so frames need no copy between
/// adapters; the default adapter serves a display that no adapter reports.
fn device_for(monitor: HMONITOR) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    // SAFETY: enumeration only reads descriptions through the live interfaces DXGI returns.
    let adapter = unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        (0..)
            .map_while(|index| factory.EnumAdapters(index).ok())
            .find(|adapter| {
                (0..)
                    .map_while(|index| adapter.EnumOutputs(index).ok())
                    .any(|output| {
                        output
                            .GetDesc()
                            .is_ok_and(|description| description.Monitor == monitor)
                    })
            })
    };
    let driver = if adapter.is_some() {
        D3D_DRIVER_TYPE_UNKNOWN
    } else {
        D3D_DRIVER_TYPE_HARDWARE
    };
    let (mut device, mut context) = (None, None);
    // SAFETY: any adapter is live, and both out-parameters are live locals.
    unsafe {
        D3D11CreateDevice(
            adapter.as_ref(),
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&raw mut device),
            None,
            Some(&raw mut context),
        )?;
    }
    Ok((created(device)?, created(context)?))
}

/// Reads frames of one size back to memory, halved on the GPU until they fit.
struct Readback {
    /// The size of the frames read.
    source: [u32; 2],
    /// The size delivered: that of the first mip level whose long edge fits.
    size: [u32; 2],
    level: u32,
    /// The frame and its halvings, while it needs halving.
    mips: Option<Mips>,
    /// A CPU-readable copy of the delivered level, reused frame after frame.
    staging: ID3D11Texture2D,
}

struct Mips {
    texture: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
}

impl Readback {
    fn new(
        device: &ID3D11Device,
        source: [u32; 2],
        longest_edge: u32,
    ) -> windows::core::Result<Self> {
        let size = fit(source, longest_edge);
        let level = halvings(source, size);
        let mips = if level == 0 {
            None
        } else {
            Some(Mips::new(device, source, level)?)
        };
        let staging = create_texture(
            device,
            &D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0.cast_unsigned(),
                ..texture_description(size, 1)
            },
        )?;
        Ok(Self {
            source,
            size,
            level,
            mips,
            staging,
        })
    }

    /// Copies `frame`, which must be of the source size, to memory as tightly packed BGRA rows of
    /// the delivered size.
    fn read(
        &self,
        context: &ID3D11DeviceContext,
        frame: &ID3D11Texture2D,
    ) -> windows::core::Result<Vec<u8>> {
        let [width, height] = self.source;
        let region = D3D11_BOX {
            right: width,
            bottom: height,
            back: 1,
            ..Default::default()
        };
        let first = self
            .mips
            .as_ref()
            .map_or(&self.staging, |mips| &mips.texture);
        // SAFETY: every texture belongs to this context's device and shares the frame's format,
        // and the region spans the frame, whose size is the source size.
        unsafe {
            context.CopySubresourceRegion(first, 0, 0, 0, 0, frame, 0, Some(&raw const region));
            if let Some(mips) = &self.mips {
                context.GenerateMips(&mips.view);
                let (staging, halved, level) = (&self.staging, &mips.texture, self.level);
                context.CopySubresourceRegion(staging, 0, 0, 0, 0, halved, level, None);
            }
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging texture allows CPU reads; mapping waits for the copies above.
        unsafe { context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))? };
        let packed = self.pack(&mapped);
        // SAFETY: the texture is mapped, and `pack` no longer borrows its bytes.
        unsafe { context.Unmap(&self.staging, 0) };
        packed
    }

    fn pack(&self, mapped: &D3D11_MAPPED_SUBRESOURCE) -> windows::core::Result<Vec<u8>> {
        let [width, height] = self.size.map(|edge| edge as usize);
        let row = width.saturating_mul(BYTES_PER_PIXEL);
        let pitch = mapped.RowPitch as usize;
        let Some(length) =
            mapped_len(pitch, row, height).filter(|_| pitch >= row && !mapped.pData.is_null())
        else {
            return Err(E_UNEXPECTED.into());
        };
        // SAFETY: Direct3D maps `height` rows starting `pitch` bytes apart, valid until the caller
        // unmaps them, and `length` ends with the last row's pixels.
        let bytes = unsafe { slice::from_raw_parts(mapped.pData.cast::<u8>(), length) };
        pack_rows(bytes, pitch, width, height).ok_or_else(|| E_UNEXPECTED.into())
    }
}

impl Mips {
    fn new(device: &ID3D11Device, source: [u32; 2], level: u32) -> windows::core::Result<Self> {
        let texture = create_texture(
            device,
            &D3D11_TEXTURE2D_DESC {
                BindFlags: (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET)
                    .0
                    .cast_unsigned(),
                MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0.cast_unsigned(),
                ..texture_description(source, level.saturating_add(1))
            },
        )?;
        let mut view = None;
        // SAFETY: the texture is live and the out-parameter outlives the call.
        unsafe { device.CreateShaderResourceView(&texture, None, Some(&raw mut view))? };
        Ok(Self {
            texture,
            view: created(view)?,
        })
    }
}

/// A single-sampled BGRA texture of `size` with `levels` mip levels.
fn texture_description([width, height]: [u32; 2], levels: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: levels,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        ..Default::default()
    }
}

fn create_texture(
    device: &ID3D11Device,
    description: &D3D11_TEXTURE2D_DESC,
) -> windows::core::Result<ID3D11Texture2D> {
    let mut texture = None;
    // SAFETY: the description and out-parameter outlive the call, which reads no initial data.
    unsafe { device.CreateTexture2D(description, None, Some(&raw mut texture))? };
    created(texture)
}

/// A creation call that succeeds always fills its out-parameter.
fn created<T>(value: Option<T>) -> windows::core::Result<T> {
    value.ok_or_else(|| E_UNEXPECTED.into())
}

/// How many times `fit` halved `source` to reach `fitted`, which is the mip level holding it.
fn halvings([width, _]: [u32; 2], [fitted, _]: [u32; 2]) -> u32 {
    width
        .checked_div(fitted)
        .and_then(u32::checked_ilog2)
        .unwrap_or(0)
}

/// The bytes a mapping of `rows` rows must hold: every row but the last spans the full pitch.
fn mapped_len(pitch: usize, row: usize, rows: usize) -> Option<usize> {
    pitch.checked_mul(rows.checked_sub(1)?)?.checked_add(row)
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

pub(crate) fn configure_overlay(handle: RawWindowHandle, display: Display) -> anyhow::Result<()> {
    let hwnd = hwnd_of(handle)?;
    // First, so a build that cannot exclude the overlay leaves the window untouched.
    exclude_always(hwnd)?;
    // The overlay is the pill's click-through popup, stretched over the display.
    configure_pill(handle)?;
    let [left, top, ..] = display.bounds;
    let [width, height] = display.extent();
    let place = || {
        // SAFETY: the caller passes a live window on its UI thread.
        ok_or_last_error(unsafe {
            SetWindowPos(hwnd, HWND_TOPMOST, left, top, width, height, SWP_NOACTIVATE)
        })
    };
    // SAFETY: as above.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    place()?;
    // Arriving on a display of another DPI makes GPUI resize the window to the size Windows
    // suggests; placing it again from that display lands it on the exact bounds.
    // SAFETY: as above.
    if unsafe { GetDpiForWindow(hwnd) } != dpi {
        place()?;
    }
    Ok(())
}

/// Leaves `hwnd` out of all screen capture for good, marked so no capture ending restores it.
fn exclude_always(hwnd: HWND) -> anyhow::Result<()> {
    ensure!(
        windows_build().is_some_and(|build| build >= EXCLUSION_BUILD),
        "Shows need Windows 10 version 2004 or later"
    );
    // SAFETY: the caller passes a live window this process owns on its UI thread. The subclass
    // removes the mark before the window is destroyed, as properties require.
    unsafe {
        ok_or_last_error(SetPropW(hwnd, ALWAYS_EXCLUDED, hwnd.cast()))?;
        ok_or_last_error(SetWindowSubclass(hwnd, Some(always_excluded), 1, 0))?;
        ok_or_last_error(SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE))
            .context("Cannot keep the Show overlay out of screen capture")
    }
}

unsafe extern "system" fn always_excluded(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    // SAFETY: this window-owned subclass runs on the window's thread and is removed, with the
    // mark, at destruction.
    unsafe {
        if message == WM_NCDESTROY {
            RemovePropW(hwnd, ALWAYS_EXCLUDED);
            RemoveWindowSubclass(hwnd, Some(always_excluded), id);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

pub(crate) fn set_overlay_visible(handle: RawWindowHandle, visible: bool) {
    let Ok(hwnd) = hwnd_of(handle) else {
        return;
    };
    // SAFETY: the caller passes a live window on its UI thread.
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

/// The running Windows build, read from the kernel: the documented version APIs report Windows 8
/// to executables whose manifests list no newer version.
fn windows_build() -> Option<u32> {
    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: u32::try_from(size_of::<OSVERSIONINFOW>()).ok()?,
        ..Default::default()
    };
    // SAFETY: RtlGetVersion fills this live structure, whose size field it checks.
    let status = unsafe { RtlGetVersion(&raw mut version) };
    (status >= 0).then_some(version.dwBuildNumber)
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
            bounds: [-3840, -794, 0, 1366],
        };
        let at = |x: i32, y: i32| display.area().normalize([x, y].map(f64::from));
        assert_eq!(at(-1920, 286), Some([0.5, 0.5]));
        assert_eq!(
            at(960, 1366),
            Some([1.25, 1.0]),
            "A pointer on the next display must not clamp onto this one's edge"
        );
        let desktop = Display {
            monitor: 0,
            bounds: [i32::MIN, i32::MIN, i32::MAX, i32::MAX],
        };
        assert_eq!(
            desktop
                .area()
                .normalize([i32::MAX, i32::MIN].map(f64::from)),
            Some([1.0, 0.0]),
            "Spans across the whole coordinate range must not overflow"
        );
    }

    #[test]
    fn fitted_frames_are_the_mip_level_their_halvings_name() {
        for (source, longest_edge, level) in [
            ([1920, 1080], 1920, 0),
            ([3840, 2160], 2560, 1),
            ([3840, 2160], 960, 2),
            ([1921, 1081], 960, 1),
            ([1080, 1920], 960, 1),
            ([4096, 3], 16, 1),
        ] {
            let fitted = fit(source, longest_edge);
            assert_eq!(
                halvings(source, fitted),
                level,
                "{source:?} fitted to {fitted:?}"
            );
            assert_eq!(
                fitted,
                source.map(|edge| edge >> level),
                "The staging texture must match mip level {level} of {source:?}"
            );
        }
    }

    #[test]
    fn a_mapping_ends_with_its_last_row_of_pixels() {
        // Two rows of two pixels, each starting 12 bytes after the last; the last row is unpadded.
        let mapped = [1, 1, 1, 1, 2, 2, 2, 2, 9, 9, 9, 9, 3, 3, 3, 3, 4, 4, 4, 4];
        assert_eq!(mapped_len(12, 8, 2), Some(mapped.len()));
        assert_eq!(
            pack_rows(&mapped, 12, 2, 2),
            Some(vec![1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4])
        );
        assert_eq!(mapped_len(12, 8, 0), None, "An empty image maps nothing");
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
