//! The display a Show records: its frames, the pointer over it, and the overlay drawn on it.
//!
//! Frames leave out Speakeasy's own windows, and pointer samples are normalized to the same
//! display, so ink drawn on the overlay lines up with the frames it annotates.

#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(crate) mod geometry;

use std::{
    thread,
    time::{Duration, Instant},
};

use async_channel::Sender;

#[cfg(target_os = "linux")]
use self::unsupported as native;
#[cfg(target_os = "macos")]
use crate::macos::screen as native;
#[cfg(target_os = "windows")]
use crate::windows::screen as native;
use crate::{
    NativeWindow,
    owned_thread::{OwnedThread, StoppableThread},
};

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
    /// Shift was held, alone or beside the held talk shortcut, which draws ink.
    pub drawing: bool,
    /// The primary button was down.
    pub pressed: bool,
}

/// Captures the display under the pointer on an owned thread until stopped.
pub struct ScreenCapture {
    display: Display,
    thread: StoppableThread,
}

impl ScreenCapture {
    /// Starts capturing the display under the pointer.
    ///
    /// Frames reach `events` at most once per `interval`, each halved in size until its long edge
    /// fits `longest_edge`. No frame waits behind a full lane: Windows holds back its newest
    /// frame until there is room or a newer one replaces it, and macOS drops it. No frame shows a
    /// Speakeasy window, including one opened while capturing; they still appear in the user's own
    /// screen sharing before and after.
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
        let thread = OwnedThread::spawn_stoppable("screen-capture", move |stopping| {
            native::capture(display, interval, longest_edge, &events, &stopping);
        })?;
        Ok(Self { display, thread })
    }

    /// The display being captured.
    #[must_use]
    pub const fn display(&self) -> Display {
        self.display
    }

    /// Asks the capture to stop; it releases the display before [`stopped`](Self::stopped)
    /// resolves.
    pub fn request_stop(&self) {
        self.thread.request_stop();
    }

    /// Resolves once the capture has released the display.
    pub fn stopped(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        self.thread.stopped()
    }
}

/// Samples the pointer over a display on an owned thread until stopped.
pub struct PointerMonitor {
    thread: StoppableThread,
}

impl PointerMonitor {
    /// Samples the pointer over `display` every 8 ms, sending a sample whenever its position,
    /// drawing state, or primary button changes. Polling needs no input hook or permission, and
    /// never delays the pointer.
    ///
    /// # Errors
    ///
    /// Fails when the thread cannot start.
    pub fn start(display: Display, samples: Sender<PointerSample>) -> anyhow::Result<Self> {
        let thread = OwnedThread::spawn_stoppable("pointer", move |stopping| {
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
        Ok(Self { thread })
    }

    /// Asks the monitor to stop.
    pub fn request_stop(&self) {
        self.thread.request_stop();
    }

    /// Resolves once the monitor has stopped.
    pub fn stopped(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        self.thread.stopped()
    }
}

impl PointerSample {
    fn differs(&self, other: &Self) -> bool {
        (self.x, self.y, self.drawing, self.pressed)
            != (other.x, other.y, other.drawing, other.pressed)
    }
}

impl NativeWindow {
    /// Makes this window an overlay over `display` that never takes input or focus, stays above
    /// other windows, and is left out of screen capture.
    ///
    /// # Errors
    ///
    /// Fails when the platform cannot keep the window out of screen capture or place it.
    pub fn configure_overlay(&self, display: Display) -> anyhow::Result<()> {
        native::configure_overlay(self, display.native)
    }

    /// Shows or hides the overlay without activating it.
    pub fn set_overlay_visible(&self, visible: bool) {
        native::set_overlay_visible(self, visible);
    }
}

/// Linux records no Shows yet: finding a display and configuring an overlay fail, and the other
/// entry points do nothing.
#[cfg(target_os = "linux")]
mod unsupported {
    use std::{convert::Infallible, time::Duration};

    use anyhow::bail;
    use async_channel::{Receiver, Sender};

    use super::{PointerSample, ScreenEvent};
    use crate::NativeWindow;

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

    pub(super) fn configure_overlay(_: &NativeWindow, _: Display) -> anyhow::Result<()> {
        bail!("Shows are not available on Linux yet")
    }

    pub(super) const fn set_overlay_visible(_: &NativeWindow, _: bool) {}
}
