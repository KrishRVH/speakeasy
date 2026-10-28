//! Screen capture, pointer sampling, and the Show overlay on macOS.
//!
//! Capture polls `ScreenCaptureKit` for stills of one display with Speakeasy's own application
//! filtered out, so neither the pill nor the overlay ever appears in a frame. A Show of part of the
//! display asks each still for that part only, looking up a window's place in Core Graphics' window
//! list before each still. `ScreenCaptureKit` answers on queues of its own: each answer is read
//! where it arrives and crosses to the capture thread over a channel, and the capture thread stops
//! waiting the moment the Show stops.

use std::{
    convert::Infallible,
    fmt,
    panic::{self, AssertUnwindSafe},
    ptr::NonNull,
    slice,
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail};
use async_channel::{Receiver, Sender};
use block2::{DynBlock, RcBlock};
use objc2::{
    AllocAnyThread, RefEncode,
    rc::{Retained, autoreleasepool},
};
use objc2_app_kit::{
    NSPanel, NSStatusWindowLevel, NSWindowCollectionBehavior, NSWindowSharingType,
    NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect,
    CGSize,
};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGError, CGEvent, CGEventSource, CGEventSourceStateID,
    CGGetDisplaysWithPoint, CGMainDisplayID, CGMouseButton, CGRectMakeWithDictionaryRepresentation,
    CGWindowID, CGWindowListCopyWindowInfo, CGWindowListOption, kCGColorSpaceSRGB,
    kCGNullDirectDisplay, kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowIsOnscreen,
    kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerPID,
};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelFormatType_32BGRA, kCVReturnSuccess,
};
use objc2_foundation::{NSArray, NSError, NSPoint, NSRect, NSSize};
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
    SCStreamErrorCode, SCStreamErrorDomain,
};
use tokio::{runtime, time};

use crate::{
    NativeWindow,
    screen::{
        self, Frame, Part, PointerSample, Region, ScreenEvent, Target,
        geometry::{Bounds, fit, narrow, pack_rows, pixels},
    },
};

/// How long `ScreenCaptureKit` may take to answer, and capture may go without a frame, before the
/// Show reports that capture stopped.
const PATIENCE: Duration = Duration::from_secs(5);

/// Why a capture ended early, worded for the person recording.
#[derive(Clone, Copy, Debug)]
enum Failure {
    Permission,
    Stalled,
    Unstarted,
    DisplayLost,
    Exposed,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Permission => {
                "Allow Screen & System Audio Recording for Speakeasy in System Settings › Privacy & Security. If it is already on, remove it with − and add it again, then try again."
            },
            Self::Stalled => "Screen capture stopped responding. Start the Show again.",
            Self::Unstarted => "Screen capture could not start. Start the Show again.",
            Self::DisplayLost => {
                "The display being shown is no longer available. Start the Show again."
            },
            Self::Exposed => {
                "Speakeasy could not keep its own windows out of the Show. Start the Show again."
            },
        })
    }
}

/// A display and where it lies in Core Graphics' global space: points measured from the top-left
/// corner of the main display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Display {
    id: CGDirectDisplayID,
    bounds: Bounds,
}

/// A window by its number in Core Graphics' window list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window(CGWindowID);

/// One window as Core Graphics' window list describes it.
struct Described {
    number: CGWindowID,
    owner: i64,
    /// The window's level: zero for ordinary app windows; the Dock, menus, and the menu bar sit
    /// higher.
    layer: i64,
    alpha: f64,
    on_screen: bool,
    bounds: Bounds,
}

/// The frontmost visible app window under the pointer and the part of `display` it covers, unless
/// it is Speakeasy's own. Windows above app windows, such as the Dock, menus, and the menu bar, and
/// invisible windows are passed over.
pub(crate) fn window_under_pointer(display: Display) -> Option<(Window, Part)> {
    let pointer = pointer_location()?;
    let options =
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let front = windows(options, kCGNullWindowID)
        .into_iter()
        .find(|window| window.layer == 0 && window.alpha > 0.0 && window.bounds.contains(pointer))
        .filter(|window| !super::is_own_process(window.owner))?;
    Some((
        Window(front.number),
        display.bounds.part_covered_by(front.bounds)?,
    ))
}

/// The part of `display` that `window` covers now; `None` once it is closed, minimized, hidden,
/// or off the display.
fn window_part(display: Display, window: Window) -> Option<Part> {
    let described = windows(CGWindowListOption::OptionIncludingWindow, window.0)
        .into_iter()
        .find(|described| described.number == window.0 && described.on_screen)?;
    display.bounds.part_covered_by(described.bounds)
}

/// The windows Core Graphics lists for `options` relative to `window`, front to back.
fn windows(options: CGWindowListOption, window: CGWindowID) -> Vec<Described> {
    let Some(list) = CGWindowListCopyWindowInfo(options, window) else {
        return Vec::new();
    };
    // SAFETY: Core Graphics documents the window list as an array of dictionaries keyed by
    // strings, and the retained array keeps its elements alive.
    let list =
        unsafe { CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFType>>>(list) };
    list.iter().filter_map(|window| describe(&window)).collect()
}

fn describe(window: &CFDictionary<CFString, CFType>) -> Option<Described> {
    // SAFETY: the keys are immutable constants Core Graphics exports.
    let [id, owner, layer, alpha, on_screen, bounds] = unsafe {
        [
            kCGWindowNumber,
            kCGWindowOwnerPID,
            kCGWindowLayer,
            kCGWindowAlpha,
            kCGWindowIsOnscreen,
            kCGWindowBounds,
        ]
    }
    .map(|key| window.get(key));
    let numeric = |value: Option<CFRetained<CFType>>| value?.downcast::<CFNumber>().ok();
    let mut rect = CGRect::default();
    let bounds = bounds?.downcast::<CFDictionary>().ok()?;
    // SAFETY: the dictionary is alive for the call, and the rectangle is a live local.
    if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(&bounds), &raw mut rect) } {
        return None;
    }
    Some(Described {
        number: u32::try_from(numeric(id)?.as_i64()?).ok()?,
        owner: numeric(owner)?.as_i64()?,
        layer: numeric(layer)?.as_i64()?,
        alpha: numeric(alpha)
            .and_then(|alpha| alpha.as_f64())
            .unwrap_or(1.0),
        on_screen: on_screen
            .and_then(|value| value.downcast::<CFBoolean>().ok())
            .is_some_and(|value| value.as_bool()),
        bounds: Bounds::from(rect),
    })
}

pub(crate) fn display_under_pointer() -> anyhow::Result<screen::Display> {
    let [x, y] = pointer_location().context("Cannot read the pointer position. Try again.")?;
    let mut id = kCGNullDirectDisplay;
    let mut found = 0;
    // SAFETY: both out-pointers name live locals, with room for the one display requested.
    let status =
        unsafe { CGGetDisplaysWithPoint(NSPoint::new(x, y), 1, &raw mut id, &raw mut found) };
    if status != CGError::Success || found == 0 {
        bail!("No display is under the pointer. Move it onto a screen and try again.");
    }
    let bounds = display_bounds(id);
    Ok(screen::Display {
        native: Display { id, bounds },
        size: bounds.size.map(narrow),
    })
}

pub(crate) fn capture(
    target: Target,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
    stopping: &Receiver<Infallible>,
) {
    let Ok(runtime) = runtime::Builder::new_current_thread().enable_time().build() else {
        // Nothing has been sent yet, so the lane has room for the failure.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A closed lane means nobody is waiting for this capture"
        )]
        let _ = events.try_send(ScreenEvent::Failed(Failure::Unstarted.to_string()));
        return;
    };
    runtime.block_on(async {
        let failed = async {
            let Err(failure) = capture_stills(target, interval, longest_edge, events).await;
            // Unlike a frame, the failure waits for room: it is the capture's last word.
            events.send(ScreenEvent::Failed(failure.to_string())).await
        };
        tokio::select! {
            _ = stopping.recv() => {},
            () = events.closed() => {},
            _ = failed => {},
        }
    });
}

/// Captures stills of `target` until capture fails, returning what the user can do about it.
async fn capture_stills(
    target: Target,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
) -> Result<Infallible, Failure> {
    let display = target.display().native;
    let source = answer(Source::fetch(display.id, longest_edge))
        .await
        .unwrap_or(Err(Failure::Stalled))?;
    let mut part = target.part();
    let mut last_frame = Instant::now();
    loop {
        if let Region::Window(window, _) = target.region
            && let Some(now) = window_part(display, window)
        {
            part = now;
        }
        // An unanswered request is a missed still, and a long enough run of misses is a stall.
        match answer(source.still(part)?).await.unwrap_or(Ok(None))? {
            Some(frame) => {
                last_frame = frame.captured;
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A frame the lane has no room for is dropped, and a closed lane ends capture"
                )]
                let _ = events.try_send(ScreenEvent::Frame(frame));
            },
            None if last_frame.elapsed() >= PATIENCE => return Err(Failure::Stalled),
            None => {},
        }
        // Waiting a whole interval after each answer keeps frames an interval apart, however long
        // ScreenCaptureKit takes to deliver them.
        time::sleep(interval).await;
    }
}

/// What to capture: the display without Speakeasy's own application, with its size in points and
/// pixels per point, which size each still to fit the longest edge.
struct Source {
    filter: Retained<SCContentFilter>,
    points: [f64; 2],
    scale: f64,
    longest_edge: u32,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "objc2 cannot know ScreenCaptureKit objects lack thread affinity; the SAFETY note explains why moving them is sound"
)]
// SAFETY: ScreenCaptureKit's filters have no thread affinity; its own API builds them on one queue
// and reads them on others. A `Source` never changes its filter once built, so the thread it moves
// to may use it while ScreenCaptureKit reads it too.
unsafe impl Send for Source {}

impl Source {
    /// Lists the shareable content, which prompts for Screen Recording the first time, and builds
    /// the capture of `display` from it.
    fn fetch(id: CGDirectDisplayID, longest_edge: u32) -> Receiver<Result<Self, Failure>> {
        ask(
            |handler| {
                // SAFETY: the handler has the documented signature and captures only sendable
                // values.
                unsafe { SCShareableContent::getShareableContentWithCompletionHandler(handler) };
            },
            move |content: Option<&SCShareableContent>, _: Option<&NSError>| {
                // Of the reasons macOS lists no content, a missing Screen Recording permission is
                // the one the user can act on, and macOS versions do not report it consistently.
                Self::new(content.ok_or(Failure::Permission)?, id, longest_edge)
            },
        )
    }

    fn new(
        content: &SCShareableContent,
        id: CGDirectDisplayID,
        longest_edge: u32,
    ) -> Result<Self, Failure> {
        // SAFETY: these accessors only read the content snapshot.
        let (display, own) = unsafe {
            let display = content
                .displays()
                .iter()
                .find(|display| display.displayID() == id);
            let own = content
                .applications()
                .iter()
                .find(|application| super::is_own_process(application.processID().into()));
            (display, own)
        };
        let display = display.ok_or(Failure::DisplayLost)?;
        let own = own.ok_or(Failure::Exposed)?;
        // Excluding the application, rather than its windows, also leaves out windows it opens
        // after the filter is built, such as the overlay.
        // SAFETY: the display and application come from the same content, as the filter expects.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::from_retained_slice(&[own]),
                &NSArray::new(),
            )
        };
        // SAFETY: a filter's content rectangle and scale are plain reads.
        let (points, scale) = unsafe { (filter.contentRect().size, filter.pointPixelScale()) };
        Ok(Self {
            filter,
            points: [points.width, points.height],
            scale: f64::from(scale),
            longest_edge,
        })
    }

    /// How to capture `part` of the display: at its size in pixels, halved until it fits the
    /// longest edge. `None` when the part comes to no pixels, as when the display is gone.
    fn configuration(&self, part: Part) -> Option<Retained<SCStreamConfiguration>> {
        let [display_width, display_height] = self.points;
        let [left, top, right, bottom] =
            [part.left, part.top, part.right, part.bottom].map(f64::from);
        let area = CGRect::new(
            CGPoint::new(left * display_width, top * display_height),
            CGSize::new(
                (right - left) * display_width,
                (bottom - top) * display_height,
            ),
        );
        let [width, height] = pixels([area.size.width, area.size.height], self.scale)
            .map(|size| fit(size, self.longest_edge))?;
        // SAFETY: a new configuration's setters take plain values and a color space name that
        // lives as long as the process.
        let configuration = unsafe {
            let configuration = SCStreamConfiguration::new();
            // In points from the display's top-left corner.
            configuration.setSourceRect(area);
            configuration.setWidth(width as usize);
            configuration.setHeight(height as usize);
            configuration.setPixelFormat(kCVPixelFormatType_32BGRA);
            // Frames carry no color profile, so they are converted to the sRGB readers assume
            // rather than left in the display's wider gamut.
            configuration.setColorSpaceName(kCGColorSpaceSRGB);
            // Click markers stand in for the cursor, which would only hide what it points at.
            configuration.setShowsCursor(false);
            // Halving can round the axes differently. Stretching by that fraction of a pixel keeps
            // each frame filled edge to edge, where letterboxing would shift ink off what it marks.
            configuration.setPreservesAspectRatio(false);
            configuration
        };
        Some(configuration)
    }

    /// Asks for one still of `part` of the display; `None` when `ScreenCaptureKit` misses it.
    ///
    /// # Errors
    ///
    /// Fails when the part comes to no pixels, as when the display is gone.
    fn still(&self, part: Part) -> Result<Receiver<Result<Option<Frame>, Failure>>, Failure> {
        let configuration = self.configuration(part).ok_or(Failure::DisplayLost)?;
        Ok(ask(
            |handler| {
                // SAFETY: the filter and configuration are alive for the call, which retains them,
                // and the handler has the documented signature and captures only sendable values.
                unsafe {
                    SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
                        &self.filter,
                        &configuration,
                        Some(handler),
                    );
                };
            },
            move |sample: Option<&CMSampleBuffer>, error: Option<&NSError>| match sample {
                Some(sample) => Ok(frame(sample, part)),
                None if error.is_some_and(refused) => Err(Failure::Permission),
                None => Ok(None),
            },
        ))
    }
}

/// Sends a `ScreenCaptureKit` request through `send`, whose completion handler `read` turns into
/// the one value the returned lane carries.
///
/// `ScreenCaptureKit` calls the handler on a queue of its own, so `read` runs there, while the
/// reply is alive, and returns something that can cross to the waiting thread.
fn ask<T, O>(
    send: impl FnOnce(&DynBlock<dyn Fn(*mut T, *mut NSError)>),
    read: impl Fn(Option<&T>, Option<&NSError>) -> O + Send + 'static,
) -> Receiver<O>
where
    T: RefEncode + 'static,
    O: Send + 'static,
{
    let (reply, replies) = async_channel::bounded(1);
    let handler = RcBlock::new(move |value: *mut T, error: *mut NSError| {
        // SAFETY: ScreenCaptureKit passes each argument as nil or an object it keeps alive for the
        // duration of this call.
        let (value, error) = unsafe { (value.as_ref(), error.as_ref()) };
        // A panicking read leaves the lane to close unanswered.
        let outcome =
            panic::catch_unwind(AssertUnwindSafe(|| autoreleasepool(|_| read(value, error))));
        if let Ok(outcome) = outcome {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The asker stops listening once it gives up, so a late answer has nowhere to go"
            )]
            let _ = reply.try_send(outcome);
        }
    });
    autoreleasepool(|_| send(&handler));
    replies
}

/// The value `replies` carries, unless `ScreenCaptureKit` takes longer than [`PATIENCE`] to answer.
async fn answer<O>(replies: Receiver<O>) -> Option<O> {
    time::timeout(PATIENCE, replies.recv()).await.ok()?.ok()
}

/// Copies the pixels out of a `ScreenCaptureKit` still of `part` of the display, which carries a
/// pixel buffer in the configured format; reading it in place costs one copy, where a still image
/// would first be copied whole by its data provider.
fn frame(sample: &CMSampleBuffer, part: Part) -> Option<Frame> {
    let captured = Instant::now();
    // SAFETY: ScreenCaptureKit keeps the sample alive for its completion handler, which calls this.
    let buffer = unsafe { sample.image_buffer() }?;
    if CVPixelBufferGetPixelFormatType(&buffer) != kCVPixelFormatType_32BGRA {
        return None;
    }
    let width = CVPixelBufferGetWidth(&buffer);
    let height = CVPixelBufferGetHeight(&buffer);
    let locked = Locked::new(&buffer)?;
    let bgra = pack_rows(
        locked.rows()?,
        CVPixelBufferGetBytesPerRow(&buffer),
        width,
        height,
    )?;
    Some(Frame {
        captured,
        width: width.try_into().ok()?,
        height: height.try_into().ok()?,
        bgra,
        part,
    })
}

/// A pixel buffer whose memory stays readable until this is dropped.
struct Locked<'a>(&'a CVPixelBuffer);

impl<'a> Locked<'a> {
    fn new(pixels: &'a CVPixelBuffer) -> Option<Self> {
        // SAFETY: the buffer is alive, and `Drop` releases this lock with the same flags.
        let status =
            unsafe { CVPixelBufferLockBaseAddress(pixels, CVPixelBufferLockFlags::ReadOnly) };
        (status == kCVReturnSuccess).then_some(Self(pixels))
    }

    /// The buffer's memory: `height` rows of `bytesPerRow` bytes each.
    fn rows(&self) -> Option<&[u8]> {
        let base = NonNull::new(CVPixelBufferGetBaseAddress(self.0).cast::<u8>())?;
        let length =
            CVPixelBufferGetBytesPerRow(self.0).checked_mul(CVPixelBufferGetHeight(self.0))?;
        // SAFETY: while locked, the buffer's base address starts `height` readable rows of
        // `bytesPerRow` bytes, and the slice cannot outlive the lock it borrows.
        Some(unsafe { slice::from_raw_parts(base.as_ptr(), length) })
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: this lock was taken with the same flags.
        unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly) };
    }
}

/// Whether `ScreenCaptureKit` failed because Screen Recording is not allowed.
fn refused(error: &NSError) -> bool {
    // SAFETY: ScreenCaptureKit's error domain is an immutable constant.
    let domain = unsafe { SCStreamErrorDomain };
    *error.domain() == *domain && error.code() == SCStreamErrorCode::UserDeclined.0
}

pub(crate) fn pointer(display: Display) -> Option<PointerSample> {
    let at = Instant::now();
    let [x, y] = display.bounds.normalize(pointer_location()?)?;
    Some(PointerSample {
        at,
        x,
        y,
        drawing: super::modifiers().draws(),
        pressed: CGEventSource::button_state(
            CGEventSourceStateID::CombinedSessionState,
            CGMouseButton::Left,
        ),
    })
}

/// The pointer's position in Core Graphics' global space.
fn pointer_location() -> Option<[f64; 2]> {
    // An event made without a source carries the pointer's current position.
    let event = CGEvent::new(None)?;
    let point = CGEvent::location(Some(&event));
    Some([point.x, point.y])
}

/// Where display `id` lies in Core Graphics' global space; empty once the display is gone.
fn display_bounds(id: CGDirectDisplayID) -> Bounds {
    Bounds::from(CGDisplayBounds(id))
}

/// A Core Graphics or `AppKit` rectangle, in the space of the API that measured it.
impl From<CGRect> for Bounds {
    fn from(rect: CGRect) -> Self {
        Self {
            origin: [rect.origin.x, rect.origin.y],
            size: [rect.size.width, rect.size.height],
        }
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Overlay configuration can fail on Windows and Linux, and the facade returns one type"
)]
pub(crate) fn configure_overlay(window: &NativeWindow, display: Display) -> anyhow::Result<()> {
    let window = &window.native.ns_window;
    window.setIgnoresMouseEvents(true);
    window.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
    // A click while the overlay takes input would otherwise make it the key window, taking typing
    // from the app being recorded; GPUI's views never need it to be key.
    if let Some(panel) = window.downcast_ref::<NSPanel>() {
        panel.setBecomesKeyOnlyIfNeeded(true);
    }
    window.setHasShadow(false);
    // Shows already leave out the whole application; this asks other recorders to leave out
    // the ink too.
    window.setSharingType(NSWindowSharingType::None);
    // Above the pill, the menu bar, and status items; menus the user opens still cover the ink.
    window.setLevel(NSStatusWindowLevel.saturating_add(1));
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    // AppKit measures screens up from the bottom of the main display, Core Graphics down from its
    // top.
    let [_, main_height] = display_bounds(CGMainDisplayID()).size;
    let Bounds {
        origin: [x, y],
        size: [width, height],
    } = display.bounds.flipped(main_height);
    window.setFrame_display(
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)),
        false,
    );
    Ok(())
}

pub(crate) fn set_overlay_input(window: &NativeWindow, takes_input: bool) {
    // A nonactivating panel takes clicks without activating Speakeasy.
    window.native.ns_window.setIgnoresMouseEvents(!takes_input);
}

pub(crate) fn set_overlay_visible(window: &NativeWindow, visible: bool) {
    let window = &window.native.ns_window;
    if visible {
        window.orderFrontRegardless();
    } else {
        window.orderOut(None);
    }
}
