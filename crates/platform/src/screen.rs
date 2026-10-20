//! The display a Show records: its frames, the pointer over it, and the overlay drawn on it.
//!
//! Frames leave out Speakeasy's own windows, and pointer samples are normalized to the same
//! display, so ink drawn on the overlay lines up with the frames it annotates.

#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(crate) mod geometry;

use std::{
    convert::Infallible,
    thread,
    time::{Duration, Instant},
};

use async_channel::Sender;
use raw_window_handle::RawWindowHandle;

#[cfg(target_os = "linux")]
use self::unsupported as native;
#[cfg(target_os = "macos")]
use crate::macos::screen as native;
use crate::owned_thread::OwnedThread;
#[cfg(target_os = "windows")]
use crate::windows::screen as native;

/// How often the pointer is sampled: fast enough to catch a deliberate click.
const POINTER_INTERVAL: Duration = Duration::from_millis(8);

/// One image of the display.
pub struct Frame {
    /// When the image was on screen.
    pub captured: Instant,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Rows of BGRA or BGRX pixels, `width × 4` bytes each with no padding.
    pub bgra: Vec<u8>,
}

/// What a running capture reports.
pub enum ScreenEvent {
    /// A new image of the display.
    Frame(Frame),
    /// Capture stopped and cannot resume; the message explains what the user can do.
    Failed(String),
}

/// The display under the pointer when a Show began.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    pub(crate) native: native::Display,
    /// Width and height in logical pixels, which ink is measured against.
    pub size: [f32; 2],
}

impl Display {
    /// The display under the pointer now.
    ///
    /// # Errors
    ///
    /// Fails when the platform cannot record its screen or no display is under the pointer.
    pub fn under_pointer() -> anyhow::Result<Self> {
        native::display_under_pointer()
    }
}

/// Where the pointer was and what it was doing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerSample {
    /// When it was sampled.
    pub at: Instant,
    /// Position normalized to the display: `0.0..=1.0` spans it on each axis.
    pub x: f32,
    /// Position normalized to the display, from its top edge.
    pub y: f32,
    /// Shift alone was held, which draws ink.
    pub drawing: bool,
    /// The primary button was down.
    pub pressed: bool,
}

/// Captures the display under the pointer on an owned thread until stopped.
pub struct ScreenCapture {
    display: Display,
    /// Never sends; closing it asks the thread to stop.
    stop: Sender<Infallible>,
    thread: OwnedThread,
}

impl ScreenCapture {
    /// Starts capturing the display under the pointer.
    ///
    /// Each frame reaches `events` at most once per `interval`, halved in size until its long
    /// edge fits `longest_edge`. A frame the lane has no room for is dropped, not queued. No frame
    /// shows a Speakeasy window, including one opened while capturing; they still appear in the
    /// user's own screen sharing before and after.
    ///
    /// # Errors
    ///
    /// Fails when the platform cannot capture its screen, no display is under the pointer, or the
    /// thread cannot start. Failures after starting arrive as [`ScreenEvent::Failed`].
    pub fn start(
        interval: Duration,
        longest_edge: u32,
        events: Sender<ScreenEvent>,
    ) -> anyhow::Result<Self> {
        let display = Display::under_pointer()?;
        let (stop, stopping) = async_channel::bounded::<Infallible>(1);
        let thread = OwnedThread::spawn("screen-capture", move || {
            native::capture(display, interval, longest_edge, &events, &stopping);
        })?;
        Ok(Self {
            display,
            stop,
            thread,
        })
    }

    /// The display being captured.
    #[must_use]
    pub const fn display(&self) -> Display {
        self.display
    }

    /// Asks the capture to stop; it releases the display before [`exited`](Self::exited) resolves.
    pub fn stop(&self) {
        self.stop.close();
    }

    /// Resolves once the capture has released the display.
    pub fn exited(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        self.thread.exited()
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Samples the pointer over a display on an owned thread until stopped.
pub struct PointerMonitor {
    stop: Sender<Infallible>,
    thread: OwnedThread,
}

impl PointerMonitor {
    /// Samples the pointer over `display` every 8 ms, sending a sample whenever its position,
    /// Shift, or primary button changes. Polling needs no input hook or permission, and never
    /// delays the pointer.
    ///
    /// # Errors
    ///
    /// Fails when the thread cannot start.
    pub fn start(display: Display, samples: Sender<PointerSample>) -> anyhow::Result<Self> {
        let (stop, stopping) = async_channel::bounded::<Infallible>(1);
        let thread = OwnedThread::spawn("pointer", move || {
            let mut last: Option<PointerSample> = None;
            while !stopping.is_closed() && !samples.is_closed() {
                if let Some(sample) = native::pointer(display.native)
                    && last.is_none_or(|last| last.differs(&sample))
                    && samples.try_send(sample).is_ok()
                {
                    last = Some(sample);
                }
                thread::sleep(POINTER_INTERVAL);
            }
        })?;
        Ok(Self { stop, thread })
    }

    /// Asks the monitor to stop.
    pub fn stop(&self) {
        self.stop.close();
    }

    /// Resolves once the monitor has stopped.
    pub fn exited(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        self.thread.exited()
    }
}

impl Drop for PointerMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

impl PointerSample {
    fn differs(&self, other: &Self) -> bool {
        (self.x, self.y, self.drawing, self.pressed)
            != (other.x, other.y, other.drawing, other.pressed)
    }
}

/// Covers `display` with an overlay window that never takes input or focus, stays above other
/// windows, and is left out of screen capture. Call on the window's UI thread.
///
/// # Errors
///
/// Fails when the handle is not a native window of this platform.
pub fn configure_overlay(handle: RawWindowHandle, display: Display) -> anyhow::Result<()> {
    native::configure_overlay(handle, display.native)
}

/// Shows or hides the overlay without activating it; call on its UI thread.
pub fn set_overlay_visible(handle: RawWindowHandle, visible: bool) {
    native::set_overlay_visible(handle, visible);
}

/// Linux records Shows in a later release.
#[cfg(target_os = "linux")]
mod unsupported {
    use std::{convert::Infallible, time::Duration};

    use anyhow::bail;
    use async_channel::{Receiver, Sender};
    use raw_window_handle::RawWindowHandle;

    use super::{PointerSample, ScreenEvent};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Display;

    pub(super) fn display_under_pointer() -> anyhow::Result<super::Display> {
        bail!("Shows are not available on Linux yet")
    }

    pub(super) const fn capture(
        _: super::Display,
        _: Duration,
        _: u32,
        _: &Sender<ScreenEvent>,
        _: &Receiver<Infallible>,
    ) {
    }

    pub(super) const fn pointer(_: Display) -> Option<PointerSample> {
        None
    }

    pub(super) fn configure_overlay(_: RawWindowHandle, _: Display) -> anyhow::Result<()> {
        bail!("Shows are not available on Linux yet")
    }

    pub(super) const fn set_overlay_visible(_: RawWindowHandle, _: bool) {}
}
