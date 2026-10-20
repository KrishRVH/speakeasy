//! Screen capture, pointer sampling, and the Show overlay on Windows.
//!
//! Frames come from Windows.Graphics.Capture on a Direct3D 11 device of the adapter that drives the
//! display. The capture thread polls the frame pool itself, so no frame callback shares its state;
//! the one notice that arrives from another thread, the display going away, only closes a channel.
//!
//! Every position is in physical pixels: the manifest makes Speakeasy per-monitor DPI aware, so the
//! cursor, monitor bounds, and window placement share one coordinate space.

use std::{
    convert::Infallible,
    ffi::c_void,
    fmt,
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
        SizeInt32,
    },
    Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus,
    Win32::{
        Foundation::{E_UNEXPECTED, HMODULE},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
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
        Foundation::{FALSE, HWND, POINT, RECT},
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
        System::{
            Com::CoIncrementMTAUsage,
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            SystemInformation::OSVERSIONINFOW,
        },
        UI::{
            HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{
                VK_CONTROL, VK_LBUTTON, VK_LWIN, VK_MENU, VK_RBUTTON, VK_RWIN, VK_SHIFT,
            },
            WindowsAndMessaging::{
                GetCursorPos, GetSystemMetrics, HWND_TOPMOST, SM_SWAPBUTTON, SW_HIDE,
                SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
                SetWindowDisplayAffinity, SetWindowPos, ShowWindow, WDA_EXCLUDEFROMCAPTURE,
                WDA_NONE,
            },
        },
    },
};

use super::{BASE_DPI, configure_pill, hwnd_of, is_key_down, ok_or_last_error};
use crate::screen::{Frame, PointerSample, ScreenEvent};

/// How often the capture thread drains the frame pool, well inside the 50 ms a stop may wait.
const POLL: Duration = Duration::from_millis(10);
/// Pool buffers: one frame may wait for its turn to be sent while Windows fills the other.
const BUFFERS: i32 = 2;
/// How long the request for border-free capture may hold back the first frame.
const ACCESS_WAIT: Duration = Duration::from_millis(250);
const POOL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const BYTES_PER_PIXEL: usize = 4;
/// Windows Runtime time spans count 100 ns ticks.
const TICKS_PER_SECOND: i128 = 10_000_000;
/// The first build that leaves excluded windows out of capture; earlier builds black them out.
const EXCLUSION_BUILD: u32 = 19_041;

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
        HMONITOR(self.monitor as *mut c_void)
    }

    /// Width and height in physical pixels.
    fn extent(self) -> [i32; 2] {
        let [left, top, right, bottom] = self.bounds;
        [right.saturating_sub(left), bottom.saturating_sub(top)]
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
    let [x, y] = normalize(display.bounds, cursor()?)?;
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

/// Where `point` lies on a display with edges `bounds`, with `0.0..=1.0` spanning it on each
/// axis. A pointer on another display falls outside that range rather than onto an edge.
fn normalize([left, top, right, bottom]: [i32; 4], point: POINT) -> Option<[f32; 2]> {
    let along = |position: i32, start: i32, end: i32| {
        let span = i64::from(end).saturating_sub(i64::from(start));
        (span > 0)
            .then(|| i64::from(position).saturating_sub(i64::from(start)) as f32 / span as f32)
    };
    Some([along(point.x, left, right)?, along(point.y, top, bottom)?])
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
    let mut capture = Capture::start(display, interval, longest_edge, stopping)?;
    let mut pacer = Pacer::new(interval);
    // Windows delivers a frame only when the screen changes, so the newest unsent frame is what
    // the screen shows now. A full lane leaves it waiting rather than reading back a frame to drop.
    let mut newest = None;
    while !stopping.is_closed() && !events.is_closed() {
        if let Some(frame) = capture.newest_frame()? {
            newest = Some(frame);
        }
        let now = Instant::now();
        if pacer.due(now)
            && !events.is_full()
            && let Some(frame) = newest.take()
        {
            let frame = capture.read(&frame)?;
            if events.try_send(ScreenEvent::Frame(frame)).is_ok() {
                pacer.sent(now);
            }
        }
        thread::sleep(POLL);
    }
    Ok(())
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
    /// The size Windows delivers frames at.
    size: SizeInt32,
    longest_edge: u32,
    readback: Readback,
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
            size,
            longest_edge,
            readback,
        })
    }

    /// The newest frame Windows delivered since the last call; fails once the display or the
    /// graphics device is gone.
    fn newest_frame(&mut self) -> Result<Option<Direct3D11CaptureFrame>, Failure> {
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
            return Ok(None);
        };
        let size = frame.ContentSize()?;
        if size == self.size {
            return Ok(Some(frame));
        }
        // The display changed resolution or rotation; this frame only fills part of its surface.
        self.pool
            .Recreate(&self.direct3d, POOL_FORMAT, BUFFERS, size)?;
        self.readback = Readback::new(&self.device, pixels(size), self.longest_edge)?;
        self.size = size;
        Ok(None)
    }

    fn next_frame(&self) -> Result<Option<Direct3D11CaptureFrame>, Failure> {
        match self.pool.TryGetNextFrame() {
            Ok(frame) => Ok(Some(frame)),
            // An empty pool answers with a null frame, which surfaces as an error without a code.
            Err(error) if error.code().is_ok() => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn read(&self, frame: &Direct3D11CaptureFrame) -> Result<Frame, Failure> {
        let captured = composed_at(frame.SystemRelativeTime()?);
        // SAFETY: the frame's surface is a live texture on this capture's device.
        let texture: ID3D11Texture2D = unsafe {
            frame
                .Surface()?
                .cast::<IDirect3DDxgiInterfaceAccess>()?
                .GetInterface()?
        };
        let bgra = self.readback.read(&self.context, &texture)?;
        let [width, height] = self.readback.size;
        Ok(Frame {
            captured,
            width,
            height,
            bgra,
        })
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

/// A Direct3D 11 device on the adapter that drives `monitor`; capture fails on any other.
fn device_for(monitor: HMONITOR) -> Result<(ID3D11Device, ID3D11DeviceContext), Failure> {
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
    }
    .ok_or(Failure::NoAdapter)?;
    let (mut device, mut context) = (None, None);
    // SAFETY: the adapter is live and both out-parameters are live locals.
    unsafe {
        D3D11CreateDevice(
            Some(&adapter),
            D3D_DRIVER_TYPE_UNKNOWN,
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
        let level = mip_level(source, longest_edge);
        let size = mip_size(source, level);
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

    /// Copies `frame` to memory as tightly packed BGRA rows of the delivered size.
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
        // and the region lies within the frame.
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
        pack_rows(bytes, pitch, row, height).ok_or_else(|| E_UNEXPECTED.into())
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

/// The first mip level whose long edge fits `longest_edge`. Level 0 is the frame itself, and each
/// level halves the one before, down to a single pixel.
fn mip_level([width, height]: [u32; 2], longest_edge: u32) -> u32 {
    let longest = width.max(height);
    let last = longest.checked_ilog2().unwrap_or(0);
    (0..last)
        .find(|&level| longest >> level <= longest_edge)
        .unwrap_or(last)
}

fn mip_size(size: [u32; 2], level: u32) -> [u32; 2] {
    size.map(|edge| (edge >> level).max(1))
}

/// The bytes a mapping of `rows` rows must hold: every row but the last spans the full pitch.
fn mapped_len(pitch: usize, row: usize, rows: usize) -> Option<usize> {
    pitch.checked_mul(rows.checked_sub(1)?)?.checked_add(row)
}

/// Packs `rows` rows of `row` bytes from `mapped`, where each row starts `pitch` bytes after the
/// last; `None` when `mapped` ends early.
fn pack_rows(mapped: &[u8], pitch: usize, row: usize, rows: usize) -> Option<Vec<u8>> {
    let lines = (0..rows).map(|index| {
        let start = index.checked_mul(pitch)?;
        mapped.get(start..start.checked_add(row)?)
    });
    Some(lines.collect::<Option<Vec<_>>>()?.concat())
}

fn pixels(size: SizeInt32) -> [u32; 2] {
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
/// process. Windows' capture internals outlive the objects that start them, and tearing the
/// apartment down after a capture crashes the process.
fn join_lasting_apartment() -> windows::core::Result<()> {
    let mut cookie = null_mut();
    // SAFETY: writes only the live local. The cookie is never handed back, by design.
    HRESULT(unsafe { CoIncrementMTAUsage(&raw mut cookie) }).ok()
}

/// Why a capture ended early, worded for the person recording.
#[derive(Debug)]
enum Failure {
    /// The display was disconnected or turned off.
    DisplayLost,
    /// No graphics adapter claims the display.
    NoAdapter,
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
            Self::DisplayLost => {
                formatter.write_str("The recorded display was disconnected. Start the Show again.")
            },
            Self::NoAdapter => formatter.write_str(
                "Windows cannot capture this display. Start the Show on another display.",
            ),
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

pub(crate) fn configure_overlay(handle: RawWindowHandle, display: Display) -> anyhow::Result<()> {
    let hwnd = hwnd_of(handle)?;
    // First, so a build that cannot exclude the overlay leaves the window untouched.
    exclude_from_capture(hwnd, true)?;
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

pub(crate) fn set_excluded_from_capture(
    handle: RawWindowHandle,
    excluded: bool,
) -> anyhow::Result<()> {
    exclude_from_capture(hwnd_of(handle)?, excluded)
}

fn exclude_from_capture(hwnd: HWND, excluded: bool) -> anyhow::Result<()> {
    let affinity = if excluded {
        ensure!(
            windows_build()? >= EXCLUSION_BUILD,
            "Shows need Windows 10 version 2004 or later"
        );
        WDA_EXCLUDEFROMCAPTURE
    } else {
        WDA_NONE
    };
    // SAFETY: the caller passes a live window this process owns.
    ok_or_last_error(unsafe { SetWindowDisplayAffinity(hwnd, affinity) })
        .context("Cannot keep Speakeasy out of screen capture")
}

/// The running Windows build, read from the kernel: the documented version APIs report Windows 8
/// to executables whose manifests list no newer version.
fn windows_build() -> anyhow::Result<u32> {
    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: u32::try_from(size_of::<OSVERSIONINFOW>())?,
        ..Default::default()
    };
    // SAFETY: RtlGetVersion fills this live structure, whose size field it checks.
    let status = unsafe { RtlGetVersion(&raw mut version) };
    ensure!(status >= 0, "Cannot read the Windows version");
    Ok(version.dwBuildNumber)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_positions_normalize_to_their_display() {
        // A display left of and above the primary one, as desktop coordinates allow.
        let display = [-3840, -794, 0, 1366];
        let at = |x, y| normalize(display, POINT { x, y });
        assert_eq!(at(-3840, -794), Some([0.0, 0.0]));
        assert_eq!(at(-1920, 286), Some([0.5, 0.5]));
        assert_eq!(
            at(960, 1366),
            Some([1.25, 1.0]),
            "A pointer on the next display must not clamp onto this one's edge"
        );
        assert_eq!(normalize([0, 0, 0, 1080], POINT { x: 0, y: 0 }), None);
        let desktop = [i32::MIN, i32::MIN, i32::MAX, i32::MAX];
        assert_eq!(
            normalize(
                desktop,
                POINT {
                    x: i32::MAX,
                    y: i32::MIN
                }
            ),
            Some([1.0, 0.0]),
            "Spans across the whole coordinate range must not overflow"
        );
    }

    #[test]
    fn frames_halve_until_their_long_edge_fits() {
        assert_eq!(mip_level([1920, 1080], 1920), 0);
        assert_eq!(mip_level([3840, 2160], 2560), 1);
        assert_eq!(mip_level([3840, 2160], 960), 2);
        assert_eq!(
            mip_level([1080, 1920], 960),
            1,
            "Portrait frames fit by height"
        );
        assert_eq!(mip_level([5120, 1440], 1000), 3);
        assert_eq!(mip_size([5120, 1440], 3), [640, 180]);
        assert_eq!(mip_level([1920, 1080], 0), 10, "Halving stops at one pixel");
        assert_eq!(mip_size([1920, 1080], 10), [1, 1]);
    }

    #[test]
    fn readback_drops_row_padding() {
        // Two rows of two pixels, each starting 12 bytes after the last; the last row is unpadded.
        let mapped = [1, 1, 1, 1, 2, 2, 2, 2, 9, 9, 9, 9, 3, 3, 3, 3, 4, 4, 4, 4];
        assert_eq!(mapped_len(12, 8, 2), Some(mapped.len()));
        assert_eq!(
            pack_rows(&mapped, 12, 8, 2),
            Some(vec![1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4])
        );
        assert_eq!(pack_rows(&mapped[..19], 12, 8, 2), None);
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

    /// Checks on this machine's GPU and display. The capture check shows Speakeasy's own windows
    /// for a few seconds and inspects only pixels proven to be theirs; nothing is written, logged,
    /// or printed but counts and timings.
    mod native {
        use std::{
            num::NonZeroIsize,
            ptr::{null, null_mut},
        };

        use anyhow::bail;
        use raw_window_handle::Win32WindowHandle;
        use windows::Win32::Graphics::Direct3D11::D3D11_SUBRESOURCE_DATA;
        use windows_sys::{
            Win32::{
                Foundation::{LPARAM, LRESULT, TRUE, WPARAM},
                Graphics::{
                    Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
                    Gdi::{
                        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, CreateSolidBrush,
                        DIB_RGB_COLORS, EndPaint, EnumDisplayMonitors, HDC,
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
                        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GA_ROOT,
                        GW_HWNDPREV, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetWindow,
                        GetWindowDisplayAffinity, GetWindowLongPtrW, GetWindowRect,
                        IsWindowVisible, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
                        SWP_NOZORDER, TranslateMessage, UnregisterClassW, WM_DPICHANGED, WM_PAINT,
                        WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
                        WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WindowFromPoint,
                    },
                },
            },
            core::{BOOL, PCWSTR},
            w,
        };

        use super::*;
        use crate::ScreenCapture;

        const PATTERN_WIDTH: i32 = 640;
        const PATTERN_HEIGHT: i32 = 400;
        /// The excluded overlay's place on the pattern: left, top, right, bottom.
        const OVERLAY: [i32; 4] = [200, 120, 440, 280];
        const MAGENTA: [u8; 3] = [255, 0, 255];
        /// How far a captured channel may stray from the painted one.
        const TOLERANCE: u8 = 6;
        /// The longest the windows stay on screen, whatever happens to the test.
        const ON_SCREEN: Duration = Duration::from_secs(5);
        /// Lets DWM compose a change before it is captured.
        const COMPOSED: Duration = Duration::from_millis(300);
        const PATIENCE: Duration = Duration::from_secs(3);
        #[expect(clippy::cast_possible_truncation, reason = "The header is 40 bytes")]
        const HEADER_SIZE: u32 = size_of::<BITMAPINFOHEADER>() as u32;
        /// The shell's pen handwriting canvas spans the primary display above every app, but stays
        /// transparent unless a pen writes into a text box; ink there would fail the pixel check.
        const HANDWRITING_CANVAS: &str = "ShellHandwritingCanvas";
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
                    [0; 3]
                };
            }
            match (x < PATTERN_WIDTH / 2, y < 160) {
                (true, true) => [220, 40, 40],
                (false, true) => [40, 180, 60],
                (true, false) => [40, 80, 220],
                (false, false) => [230, 200, 40],
            }
        }

        #[tokio::test]
        #[ignore = "Shows owned windows briefly; inspects only captured pixels proven theirs"]
        async fn capture_shows_own_pattern_but_not_the_excluded_overlay() -> anyhow::Result<()> {
            per_monitor_aware()?;
            let display = display_under_pointer()?;
            let scene = Scene::open(display.native)?;
            thread::sleep(COMPOSED);
            scene.check_overlay()?;
            let full = scene.capture(display, u32::MAX).await?;
            let [width, height] = display.native.extent();
            let half = width.max(height).cast_unsigned() / 2;
            let halved = scene.capture(display, half).await?;
            set_excluded_from_capture(scene.overlay_handle()?, false)?;
            thread::sleep(COMPOSED);
            let included = scene.capture(display, u32::MAX).await?;
            drop(scene);

            let area = (PATTERN_WIDTH * PATTERN_HEIGHT).cast_unsigned() as usize;
            let [left, top, right, bottom] = OVERLAY;
            let overlay = ((right - left) * (bottom - top)).cast_unsigned() as usize;
            assert_eq!(full.tally(), Tally::exact(area), "Full-size frame");
            assert_eq!(halved.tally(), Tally::exact(area / 4), "Halved frame");
            assert_eq!(
                included.tally(),
                Tally {
                    pattern: area - overlay,
                    magenta: overlay,
                    other: 0
                },
                "The overlay must appear once exclusion is lifted, proving it was in place"
            );
            Ok(())
        }

        #[test]
        #[ignore = "Uses the GPU on synthetic frames; captures nothing"]
        fn gpu_halving_reads_back_exact_pixels() -> anyhow::Result<()> {
            // SAFETY: takes a point by value and returns the primary monitor.
            let primary =
                unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
            let (device, context) =
                device_for(HMONITOR(primary)).map_err(|failure| anyhow::anyhow!("{failure}"))?;
            for source in [[1920, 1080], [3840, 2160]] {
                let frame = synthetic_frame(&device, source)?;
                for longest_edge in [u32::MAX, 2560, 960] {
                    let readback = Readback::new(&device, source, longest_edge)?;
                    let bgra = readback.read(&context, &frame)?;
                    let [width, height] = readback.size;
                    let expected: Vec<u8> = (0..height)
                        .flat_map(|y| (0..width).map(move |x| [x, y]))
                        .flat_map(|[x, y]| block_color(x << readback.level, y << readback.level))
                        .collect();
                    assert!(bgra == expected, "{source:?} read back at {width}×{height}");
                    let [median, slowest] = cost(&readback, &context, &frame)?;
                    println!(
                        "{}×{} → {width}×{height}: median {median:.2} ms, slowest {slowest:.2} ms",
                        source[0], source[1],
                    );
                }
            }
            Ok(())
        }

        /// The median and slowest of 30 readbacks of `frame`, in milliseconds.
        fn cost(
            readback: &Readback,
            context: &ID3D11DeviceContext,
            frame: &ID3D11Texture2D,
        ) -> windows::core::Result<[f64; 2]> {
            let mut costs = Vec::new();
            for _ in 0..30 {
                let started = Instant::now();
                readback.read(context, frame)?;
                costs.push(started.elapsed());
            }
            costs.sort();
            Ok([costs[15], costs[29]].map(|cost| cost.as_secs_f64() * 1e3))
        }

        #[test]
        #[ignore = "Moves a hidden owned window across display scales; captures nothing"]
        fn overlay_covers_its_display_after_crossing_scales() -> anyhow::Result<()> {
            per_monitor_aware()?;
            let displays = displays()?;
            let scale = |display: &crate::screen::Display| {
                display.native.extent()[0] as f32 / display.size[0]
            };
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
            let handle = RawWindowHandle::Win32(Win32WindowHandle::new(
                NonZeroIsize::new(window.0 as isize).context("Missing window")?,
            ));
            configure_overlay(handle, to.native)?;
            let mut bounds = RECT::default();
            // SAFETY: reads this thread's own live window into a live local.
            ok_or_last_error(unsafe { GetWindowRect(window.0, &raw mut bounds) })?;
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
                let monitors = unsafe { &mut *(monitors as *mut Vec<isize>) };
                monitors.push(monitor as isize);
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

        /// What a crop of the pattern window holds, pixel by pixel.
        #[derive(Debug, PartialEq, Eq)]
        struct Tally {
            pattern: usize,
            magenta: usize,
            other: usize,
        }

        impl Tally {
            const fn exact(pixels: usize) -> Self {
                Self {
                    pattern: pixels,
                    magenta: 0,
                    other: 0,
                }
            }
        }

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Shade {
            Pattern,
            Magenta,
            Other,
        }

        /// How a captured BGRA pixel compares with the RGB color painted beneath it.
        fn shade([blue, green, red, _]: [u8; 4], painted: [u8; 3]) -> Shade {
            let near = |[r, g, b]: [u8; 3]| {
                [red.abs_diff(r), green.abs_diff(g), blue.abs_diff(b)]
                    .into_iter()
                    .all(|difference| difference <= TOLERANCE)
            };
            if near(painted) {
                Shade::Pattern
            } else if near(MAGENTA) {
                Shade::Magenta
            } else {
                Shade::Other
            }
        }

        /// The pattern window's pixels from one captured frame, `scale` physical pixels apiece.
        struct Crop {
            scale: usize,
            width: usize,
            bgra: Vec<[u8; 4]>,
        }

        impl Crop {
            /// Keeps only the part of `frame` showing `pattern`, then drops the frame.
            #[expect(
                clippy::arithmetic_side_effects,
                reason = "Offsets within one captured display, bounded by indexing"
            )]
            fn of(frame: Frame, display: Display, pattern: RECT) -> anyhow::Result<Self> {
                let [left, top, ..] = display.bounds;
                let [width, height] = display.extent();
                let scale = width / frame.width.cast_signed();
                ensure!(
                    scale > 0 && frame.height.cast_signed() * scale == height,
                    "The frame does not match the display"
                );
                let frame_pixels = |physical: i32| (physical / scale).cast_unsigned() as usize;
                let column = frame_pixels(pattern.left - left);
                let row = frame_pixels(pattern.top - top);
                let crop_width = frame_pixels(pattern.right - pattern.left);
                let crop_height = frame_pixels(pattern.bottom - pattern.top);
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
                    bgra,
                })
            }

            fn tally(&self) -> Tally {
                let positions = || (0..).step_by(self.scale);
                let shades: Vec<Shade> = self
                    .bgra
                    .chunks_exact(self.width)
                    .zip(positions())
                    .flat_map(|(line, y)| {
                        line.iter()
                            .zip(positions())
                            .map(move |(&pixel, x)| shade(pixel, pattern(x, y)))
                    })
                    .collect();
                let count = |wanted| shades.iter().filter(|&&shade| shade == wanted).count();
                Tally {
                    pattern: count(Shade::Pattern),
                    magenta: count(Shade::Magenta),
                    other: count(Shade::Other),
                }
            }
        }

        /// Speakeasy's own windows: a topmost test pattern and, over part of it, a magenta overlay
        /// configured as the Show's. A painter thread owns them and destroys them when the scene
        /// drops, or after `ON_SCREEN` whatever the test does.
        struct Scene {
            pattern: isize,
            overlay: isize,
            overlay_bounds: [i32; 4],
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
                // Even offsets from the display's origin keep every pattern edge in halved frames.
                let x = left + (((right - left - PATTERN_WIDTH) / 2) & !1);
                let y = top + (((bottom - top - PATTERN_HEIGHT) / 2) & !1);
                let pattern = [x, y, PATTERN_WIDTH, PATTERN_HEIGHT];
                let [overlay_left, overlay_top, overlay_right, overlay_bottom] = OVERLAY;
                let overlay = Display {
                    monitor: display.monitor,
                    bounds: [
                        x + overlay_left,
                        y + overlay_top,
                        x + overlay_right,
                        y + overlay_bottom,
                    ],
                };
                let (opened, windows) = async_channel::bounded(1);
                let (close, closing) = async_channel::bounded::<Infallible>(1);
                let painter = thread::spawn(move || paint(pattern, overlay, &opened, &closing));
                let [pattern, overlay_window] = windows.recv_blocking()??;
                Ok(Self {
                    pattern,
                    overlay: overlay_window,
                    overlay_bounds: overlay.bounds,
                    close,
                    painter: Some(painter),
                })
            }

            fn overlay_handle(&self) -> anyhow::Result<RawWindowHandle> {
                let hwnd = NonZeroIsize::new(self.overlay).context("Missing overlay window")?;
                Ok(RawWindowHandle::Win32(Win32WindowHandle::new(hwnd)))
            }

            /// Confirms `configure_overlay` placed and styled the overlay as a Show needs.
            fn check_overlay(&self) -> anyhow::Result<()> {
                let overlay = self.overlay as HWND;
                let mut bounds = RECT::default();
                let mut affinity = 0;
                // SAFETY: the painter keeps the window alive; each call writes a live local.
                let (styles, visible) = unsafe {
                    ok_or_last_error(GetWindowRect(overlay, &raw mut bounds))?;
                    ok_or_last_error(GetWindowDisplayAffinity(overlay, &raw mut affinity))?;
                    (
                        GetWindowLongPtrW(overlay, GWL_EXSTYLE),
                        IsWindowVisible(overlay) != FALSE,
                    )
                };
                let RECT {
                    left,
                    top,
                    right,
                    bottom,
                } = bounds;
                assert_eq!([left, top, right, bottom], self.overlay_bounds);
                assert_eq!(affinity, WDA_EXCLUDEFROMCAPTURE);
                let wanted = WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST;
                assert_eq!(u32::try_from(styles)? & wanted, wanted);
                assert!(visible);
                Ok(())
            }

            /// Captures one frame of `display` and keeps only the pattern window's pixels, after
            /// proving before and after that nothing else covers them.
            async fn capture(
                &self,
                display: crate::screen::Display,
                longest_edge: u32,
            ) -> anyhow::Result<Crop> {
                let pattern = self.pattern_bounds()?;
                self.check_owned(pattern)?;
                let (events, frames) = async_channel::bounded(1);
                let started = Instant::now();
                let capture =
                    ScreenCapture::start(Duration::from_millis(100), longest_edge, events)?;
                ensure!(
                    capture.display() == display,
                    "The pointer moved to another display; nothing was inspected"
                );
                let frame = match tokio::time::timeout(PATIENCE, frames.recv()).await?? {
                    ScreenEvent::Frame(frame) => frame,
                    ScreenEvent::Failed(message) => bail!(message),
                };
                println!(
                    "First {}×{} frame {:.1} ms after start",
                    frame.width,
                    frame.height,
                    started.elapsed().as_secs_f64() * 1e3
                );
                drop(capture);
                let crop = Crop::of(frame, display.native, pattern)?;
                self.check_owned(pattern)?;
                Ok(crop)
            }

            fn pattern_bounds(&self) -> anyhow::Result<RECT> {
                let mut bounds = RECT::default();
                // SAFETY: the painter keeps the window alive, and the out-parameter is a live
                // RECT of the size passed.
                let read = unsafe {
                    DwmGetWindowAttribute(
                        self.pattern as HWND,
                        DWMWA_EXTENDED_FRAME_BOUNDS.cast_unsigned(),
                        (&raw mut bounds).cast(),
                        u32::try_from(size_of::<RECT>())?,
                    )
                };
                ensure!(read >= 0, "Cannot locate the test pattern");
                Ok(bounds)
            }

            /// Proves only the scene shows within `area`: a probe every 16 pixels and along the
            /// far edges finds the pattern, and no visible window above it overlaps the area,
            /// including click-through windows the probe passes over.
            fn check_owned(&self, area: RECT) -> anyhow::Result<()> {
                let ours = [self.pattern, self.overlay];
                let xs = (area.left..area.right)
                    .step_by(16)
                    .chain([area.right.saturating_sub(1)]);
                let ys = (area.top..area.bottom)
                    .step_by(16)
                    .chain([area.bottom.saturating_sub(1)]);
                for point in ys.flat_map(|y| xs.clone().map(move |x| POINT { x, y })) {
                    // SAFETY: both calls take values and retain nothing.
                    let root = unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) };
                    ensure!(
                        ours.contains(&(root as isize)),
                        "A {} window shows at ({}, {}) over the test pattern",
                        class_of(root),
                        point.x,
                        point.y
                    );
                }
                let above = std::iter::successors(next_above(self.pattern as HWND), |&window| {
                    next_above(window)
                });
                for window in above.filter(|&window| !ours.contains(&(window as isize))) {
                    let class = class_of(window);
                    ensure!(
                        class.starts_with(HANDWRITING_CANVAS) || !shows_over(window, area),
                        "A {class} window above the test pattern overlaps it"
                    );
                }
                Ok(())
            }
        }

        /// The window just above `window` in z-order, if any.
        fn next_above(window: HWND) -> Option<HWND> {
            // SAFETY: takes a handle by value and retains nothing.
            let above = unsafe { GetWindow(window, GW_HWNDPREV) };
            (!above.is_null()).then_some(above)
        }

        /// A window's class name, which names its kind without revealing what it shows.
        fn class_of(window: HWND) -> String {
            let mut name = [0_u16; 256];
            // SAFETY: writes at most the buffer's length into this live local.
            let length = unsafe { GetClassNameW(window, name.as_mut_ptr(), 256) };
            String::from_utf16_lossy(&name[..usize::try_from(length).unwrap_or(0)])
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

        /// Whether `window` is visible on this virtual desktop and overlaps `area`.
        fn shows_over(window: HWND, area: RECT) -> bool {
            let mut cloaked = 0_u32;
            let mut bounds = RECT::default();
            // SAFETY: each call reads a window by handle into a live local of the size passed.
            unsafe {
                IsWindowVisible(window) != FALSE
                    && (DwmGetWindowAttribute(
                        window,
                        DWMWA_CLOAKED.cast_unsigned(),
                        (&raw mut cloaked).cast(),
                        4,
                    ) < 0
                        || cloaked == 0)
                    && GetWindowRect(window, &raw mut bounds) != FALSE
                    && bounds.left < area.right
                    && area.left < bounds.right
                    && bounds.top < area.bottom
                    && area.top < bounds.bottom
            }
        }

        /// Owns the scene's windows on their own thread until `closing` closes or time runs out.
        fn paint(
            [x, y, width, height]: [i32; 4],
            overlay: Display,
            opened: &Sender<anyhow::Result<[isize; 2]>>,
            closing: &Receiver<Infallible>,
        ) {
            let windows = (|| {
                let _pattern_class =
                    Class::register(PATTERN_CLASS, Some(paint_pattern), null_mut())?;
                // SAFETY: the brush becomes the class's, which Windows deletes on unregistering.
                let magenta = unsafe { CreateSolidBrush(0x00FF_00FF) };
                let _overlay_class = Class::register(OVERLAY_CLASS, Some(DefWindowProcW), magenta)?;
                let pattern = Window::create(
                    PATTERN_CLASS,
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    [x, y, width, height],
                )?;
                let overlay_window = Window::create(
                    OVERLAY_CLASS,
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    [0, 0, 1, 1],
                )?;
                // SAFETY: shows this thread's own window without activating it.
                unsafe { ShowWindow(pattern.0, SW_SHOWNOACTIVATE) };
                let handle = RawWindowHandle::Win32(Win32WindowHandle::new(
                    NonZeroIsize::new(overlay_window.0 as isize).context("Missing overlay")?,
                ));
                configure_overlay(handle, overlay)?;
                set_overlay_visible(handle, true);
                opened.try_send(Ok([pattern.0 as isize, overlay_window.0 as isize]))?;
                pump(closing);
                anyhow::Ok(())
            })();
            if let Err(error) = windows {
                // A closed lane means the test has already given up on the scene.
                drop(opened.try_send(Err(error)));
            }
        }

        fn pump(closing: &Receiver<Infallible>) {
            let deadline = Instant::now().checked_add(ON_SCREEN).unwrap();
            let mut message = MSG::default();
            while !closing.is_closed() && Instant::now() < deadline {
                // SAFETY: messages come from this thread's queue and reach only its windows.
                unsafe {
                    while PeekMessageW(&raw mut message, null_mut(), 0, 0, PM_REMOVE) != FALSE {
                        TranslateMessage(&raw const message);
                        DispatchMessageW(&raw const message);
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
        }

        struct Class(PCWSTR);

        impl Class {
            fn register(
                name: PCWSTR,
                procedure: windows_sys::Win32::UI::WindowsAndMessaging::WNDPROC,
                background: windows_sys::Win32::Graphics::Gdi::HBRUSH,
            ) -> anyhow::Result<Self> {
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
            fn create(
                class: PCWSTR,
                styles: u32,
                [x, y, width, height]: [i32; 4],
            ) -> anyhow::Result<Self> {
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
            wparam: usize,
            lparam: isize,
        ) -> isize {
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
    }
}
