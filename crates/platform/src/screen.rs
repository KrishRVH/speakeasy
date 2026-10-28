//! The display a Show records, whole or in part: its frames, the pointer over it, and the overlay
//! drawn on it.
//!
//! Frames leave out Speakeasy's own windows, and pointer samples are normalized to the same
//! display; each frame says which part of the display it shows, so ink drawn on the overlay lines
//! up with the frames it annotates.

#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(crate) mod geometry;

use std::{
    fmt, thread,
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

/// How often the pointer is sampled.
const POINTER_INTERVAL: Duration = Duration::from_millis(8);

/// What the user asks a Show to record, which becomes a [`Target`] once found under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The display under the pointer.
    Screen,
    /// The window under the pointer, following it as it moves.
    Window,
    /// An area of the display under the pointer, dragged out once the Show starts.
    Area,
}

/// A part of a display, by its edges as shares of the display's width and height from its
/// top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Part {
    /// The left edge.
    pub left: f32,
    /// The top edge.
    pub top: f32,
    /// The right edge.
    pub right: f32,
    /// The bottom edge.
    pub bottom: f32,
}

impl Part {
    /// The whole display.
    pub const WHOLE: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    /// The part with corners at two points given as shares of the display, in either order,
    /// clipped to the display.
    #[must_use]
    pub fn between([x, y]: [f32; 2], [other_x, other_y]: [f32; 2]) -> Self {
        let share = |value: f32| value.clamp(0.0, 1.0);
        Self {
            left: share(x.min(other_x)),
            top: share(y.min(other_y)),
            right: share(x.max(other_x)),
            bottom: share(y.max(other_y)),
        }
    }

    /// The share of the display's width the part spans.
    #[must_use]
    pub fn width(self) -> f32 {
        self.right - self.left
    }

    /// The share of the display's height the part spans.
    #[must_use]
    pub fn height(self) -> f32 {
        self.bottom - self.top
    }

    /// Where a point given as shares of the display lies as shares of this part; points outside
    /// the part fall outside `0.0..=1.0`, and an empty part leaves the point as it was.
    #[must_use]
    pub fn within(self, [x, y]: [f32; 2]) -> [f32; 2] {
        let share = |value: f32, start: f32, extent: f32| {
            if extent > 0.0 {
                (value - start) / extent
            } else {
                value
            }
        };
        [
            share(x, self.left, self.width()),
            share(y, self.top, self.height()),
        ]
    }
}

/// What a Show records: a display, all of it, the part a window covers as the window moves, or a
/// fixed area of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub(crate) display: Display,
    pub(crate) region: Region,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Region {
    Whole,
    /// A window, and the part of the display it covered when chosen.
    Window(native::Window, Part),
    Area(Part),
}

impl Target {
    /// All of the display under the pointer.
    ///
    /// # Errors
    ///
    /// Fails when the platform cannot record its screen or no display is under the pointer.
    pub fn screen() -> anyhow::Result<Self> {
        Ok(Self {
            display: Display::under_pointer()?,
            region: Region::Whole,
        })
    }

    /// The top app window under the pointer, on the display under the pointer; all of that
    /// display when no app window is there, such as over the desktop or the taskbar, or when the
    /// top one is Speakeasy's.
    ///
    /// # Errors
    ///
    /// Fails as [`screen`](Self::screen) does.
    pub fn window_under_pointer() -> anyhow::Result<Self> {
        let display = Display::under_pointer()?;
        let region = native::window_under_pointer(display.native)
            .map_or(Region::Whole, |(window, part)| Region::Window(window, part));
        Ok(Self { display, region })
    }

    /// The `part` of `display`.
    #[must_use]
    pub const fn area(display: Display, part: Part) -> Self {
        Self {
            display,
            region: Region::Area(part),
        }
    }

    /// The display recorded.
    #[must_use]
    pub const fn display(&self) -> Display {
        self.display
    }

    /// The part of the display recorded from the start; a window's moves with it, as each
    /// [`Frame`] reports.
    #[must_use]
    pub const fn part(&self) -> Part {
        match self.region {
            Region::Whole => Part::WHOLE,
            Region::Window(_, part) | Region::Area(part) => part,
        }
    }
}

/// One image of the display, or of the part being recorded.
pub struct Frame {
    /// When the image was on screen.
    pub captured: Instant,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Rows of BGRA or BGRX pixels, `width × 4` bytes each with no padding.
    pub bgra: Vec<u8>,
    /// The part of the display the image shows.
    pub part: Part,
}

/// Leaves out the pixels, which show the user's screen.
impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            captured,
            width,
            height,
            bgra: _,
            part,
        } = self;
        f.debug_struct("Frame")
            .field("captured", captured)
            .field("width", width)
            .field("height", height)
            .field("part", part)
            .finish_non_exhaustive()
    }
}

/// What a running capture reports.
#[derive(Debug)]
pub enum ScreenEvent {
    /// A new image of the display.
    Frame(Frame),
    /// Capture stopped and cannot resume; the message explains what the user can do.
    Failed(String),
}

/// A display that a Show records and its overlay covers.
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

impl PointerSample {
    fn differs(&self, other: &Self) -> bool {
        (self.x, self.y, self.drawing, self.pressed)
            != (other.x, other.y, other.drawing, other.pressed)
    }
}

/// Captures a [`Target`] on an owned thread until stopped.
#[derive(Debug)]
pub struct ScreenCapture {
    thread: StoppableThread,
}

impl ScreenCapture {
    /// Starts capturing `target`.
    ///
    /// Frames reach `events` at most once per `interval`, each halved in size until its long edge
    /// fits `longest_edge`. A window's frames follow it as it moves or resizes, and keep its last
    /// place while it is minimized, closed, or off the display. No frame waits behind a full lane:
    /// Windows holds back its newest frame until there is room or a newer one replaces it, and
    /// macOS drops it. No frame shows a Speakeasy window, including one opened while capturing;
    /// they still appear in the user's own screen sharing before and after.
    ///
    /// # Errors
    ///
    /// Fails when the thread cannot start. Failures after starting arrive as
    /// [`ScreenEvent::Failed`].
    pub fn start(
        target: Target,
        interval: Duration,
        longest_edge: u32,
        events: Sender<ScreenEvent>,
    ) -> anyhow::Result<Self> {
        let thread = OwnedThread::spawn_stoppable("screen-capture", move |stopping| {
            native::capture(target, interval, longest_edge, &events, &stopping);
        })?;
        Ok(Self { thread })
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
#[derive(Debug)]
pub struct PointerMonitor {
    thread: StoppableThread,
}

impl PointerMonitor {
    /// Samples the pointer over `display` often enough to catch a deliberate click, sending a
    /// sample whenever its position, drawing state, or primary button changes. Polling needs no
    /// input hook or permission, and never delays the pointer.
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

    /// Lets the overlay take pointer input, for dragging out an area to record, or passes every
    /// click through to the apps underneath again. It never takes focus either way.
    pub fn set_overlay_input(&self, takes_input: bool) {
        native::set_overlay_input(self, takes_input);
    }
}

/// Linux records no Shows yet: finding a display and configuring an overlay fail, and the other
/// entry points do nothing.
#[cfg(target_os = "linux")]
mod unsupported {
    use std::{convert::Infallible, time::Duration};

    use anyhow::bail;
    use async_channel::{Receiver, Sender};

    use super::{Part, PointerSample, ScreenEvent, Target};
    use crate::NativeWindow;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Display;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Window;

    pub(super) fn display_under_pointer() -> anyhow::Result<super::Display> {
        bail!("Shows are not available on Linux yet")
    }

    pub(super) const fn window_under_pointer(_: Display) -> Option<(Window, Part)> {
        None
    }

    pub(super) const fn capture(
        _: Target,
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

    pub(super) const fn set_overlay_input(_: &NativeWindow, _: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_map_from_a_display_into_its_part() {
        let part = Part::between([0.75, 0.5], [0.25, -0.5]);
        assert_eq!(
            part,
            Part {
                left: 0.25,
                top: 0.0,
                right: 0.75,
                bottom: 0.5
            }
        );
        assert_eq!(part.within([0.5, 0.25]), [0.5, 0.5]);
        assert_eq!(part.within([1.0, 0.75]), [1.5, 1.5], "outside the part");
        let flat = Part::between([0.0, 0.5], [1.0, 0.5]);
        assert_eq!(
            flat.within([0.3, 0.7]),
            [0.3, 0.7],
            "an empty extent maps nothing"
        );
    }

    #[test]
    fn debugging_a_frame_never_prints_its_pixels() {
        let captured = Instant::now();
        let shown = |shade: u8| {
            let frame = Frame {
                captured,
                width: 2,
                height: 2,
                bgra: vec![shade; 16],
                part: Part::WHOLE,
            };
            format!("{frame:?}")
        };
        assert_eq!(shown(0x00), shown(0xFF));
    }
}
