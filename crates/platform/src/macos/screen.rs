//! Screen capture, pointer sampling, and the Show overlay on macOS.
//!
//! Capture polls `ScreenCaptureKit` for stills of one display with Speakeasy's own application
//! filtered out, so neither the pill nor the overlay ever appears in a frame. `ScreenCaptureKit`
//! answers on queues of its own: each answer is read where it arrives and crosses to the capture
//! thread over a channel, and the capture thread stops waiting the moment the Show stops.

use std::{
    convert::Infallible,
    panic::{self, AssertUnwindSafe},
    process, slice,
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
    NSStatusWindowLevel, NSWindowCollectionBehavior, NSWindowSharingType, NSWindowStyleMask,
};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGError, CGEvent, CGEventFlags, CGEventSource,
    CGEventSourceStateID, CGGetDisplaysWithPoint, CGMainDisplayID, CGMouseButton,
    kCGColorSpaceSRGB, kCGNullDirectDisplay,
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
use raw_window_handle::RawWindowHandle;
use tokio::{runtime, time};

use super::appkit_window;
use crate::screen::{
    self, Frame, PointerSample, ScreenEvent,
    geometry::{Bounds, fit, narrow, pack_rows, pixels},
};

/// How long `ScreenCaptureKit` may take to answer, and capture may go without a frame, before the
/// Show reports that capture stopped.
const PATIENCE: Duration = Duration::from_secs(5);

const PERMISSION: &str = "Allow Screen & System Audio Recording for Speakeasy in System Settings › Privacy & Security. If it is already on, remove it with − and add it again, then try again.";
const STALLED: &str = "Screen capture stopped responding. Start the Show again.";
const UNSTARTED: &str = "Screen capture could not start. Start the Show again.";
const DISPLAY_GONE: &str = "The display being shown is no longer available. Start the Show again.";
const EXPOSED: &str =
    "Speakeasy could not keep its own windows out of the Show. Start the Show again.";

/// A display and where it lies in Core Graphics' global space: points measured from the top-left
/// corner of the main display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Display {
    id: CGDirectDisplayID,
    bounds: Bounds,
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
    display: screen::Display,
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
        let _ = events.try_send(ScreenEvent::Failed(UNSTARTED.to_owned()));
        return;
    };
    runtime.block_on(async {
        let failed = async {
            let Err(failure) = capture_stills(display.native, interval, longest_edge, events).await;
            // Unlike a frame, the failure waits for room: it is the capture's last word.
            events.send(ScreenEvent::Failed(failure.to_owned())).await
        };
        tokio::select! {
            _ = stopping.recv() => {},
            () = events.closed() => {},
            _ = failed => {},
        }
    });
}

/// Captures stills of `display` until capture fails, returning what the user can do about it.
async fn capture_stills(
    display: Display,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
) -> Result<Infallible, &'static str> {
    let source = answer(Source::fetch(display.id, longest_edge))
        .await
        .unwrap_or(Err(STALLED))?;
    let mut last_frame = Instant::now();
    loop {
        // An unanswered request is a missed still, and a long enough run of misses is a stall.
        match answer(source.still()).await.unwrap_or(Ok(None))? {
            Some(frame) => {
                last_frame = frame.captured;
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A frame the lane has no room for is dropped, and a closed lane ends capture"
                )]
                let _ = events.try_send(ScreenEvent::Frame(frame));
            },
            None if last_frame.elapsed() >= PATIENCE => return Err(STALLED),
            None => {},
        }
        // Waiting a whole interval after each answer keeps frames an interval apart, however long
        // ScreenCaptureKit takes to deliver them.
        time::sleep(interval).await;
    }
}

/// What to capture and how: the display without Speakeasy's own application, at the Show's frame
/// size.
struct Source {
    filter: Retained<SCContentFilter>,
    configuration: Retained<SCStreamConfiguration>,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "objc2 cannot know ScreenCaptureKit objects lack thread affinity; the SAFETY note explains why moving them is sound"
)]
// SAFETY: ScreenCaptureKit's filters and configurations have no thread affinity; its own API builds
// them on one queue and reads them on others. A `Source` never changes them once built, so the
// thread it moves to may use them while ScreenCaptureKit reads them too.
unsafe impl Send for Source {}

impl Source {
    /// Lists the shareable content, which prompts for Screen Recording the first time, and builds
    /// the capture of `display` from it.
    fn fetch(id: CGDirectDisplayID, longest_edge: u32) -> Receiver<Result<Self, &'static str>> {
        ask(
            |handler| {
                // SAFETY: the handler has the documented signature and captures only sendable
                // values.
                unsafe { SCShareableContent::getShareableContentWithCompletionHandler(handler) };
            },
            move |content: Option<&SCShareableContent>, _: Option<&NSError>| {
                // Of the reasons macOS lists no content, a missing Screen Recording permission is
                // the one the user can act on, and macOS has not always reported it the same way.
                Self::new(content.ok_or(PERMISSION)?, id, longest_edge)
            },
        )
    }

    fn new(
        content: &SCShareableContent,
        id: CGDirectDisplayID,
        longest_edge: u32,
    ) -> Result<Self, &'static str> {
        // SAFETY: these accessors only read the content snapshot.
        let (display, own) = unsafe {
            let display = content
                .displays()
                .iter()
                .find(|display| display.displayID() == id);
            let own = content
                .applications()
                .iter()
                .find(|application| i64::from(application.processID()) == i64::from(process::id()));
            (display, own)
        };
        let display = display.ok_or(DISPLAY_GONE)?;
        let own = own.ok_or(EXPOSED)?;
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
        let [width, height] = pixels([points.width, points.height], f64::from(scale))
            .map(|size| fit(size, longest_edge))
            .ok_or(DISPLAY_GONE)?;
        // SAFETY: a new configuration's setters take plain values and a color space name that
        // lives as long as the process.
        let configuration = unsafe {
            let configuration = SCStreamConfiguration::new();
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
        Ok(Self {
            filter,
            configuration,
        })
    }

    /// Asks for one still of the display; `None` when `ScreenCaptureKit` misses it.
    fn still(&self) -> Receiver<Result<Option<Frame>, &'static str>> {
        ask(
            |handler| {
                // SAFETY: the filter and configuration are alive for the call, which retains them,
                // and the handler has the documented signature and captures only sendable values.
                unsafe {
                    SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
                        &self.filter,
                        &self.configuration,
                        Some(handler),
                    );
                };
            },
            |sample: Option<&CMSampleBuffer>, error: Option<&NSError>| match sample {
                Some(sample) => Ok(frame(sample)),
                None if error.is_some_and(refused) => Err(PERMISSION),
                None => Ok(None),
            },
        )
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
        // A panic must not unwind into ScreenCaptureKit; the lane then closes unanswered.
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

/// Copies the pixels out of a `ScreenCaptureKit` still, which carries a pixel buffer in the
/// configured format; reading it in place costs one copy, where a still image would first be copied
/// whole by its data provider.
fn frame(sample: &CMSampleBuffer) -> Option<Frame> {
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
        let base = CVPixelBufferGetBaseAddress(self.0).cast::<u8>();
        let length =
            CVPixelBufferGetBytesPerRow(self.0).checked_mul(CVPixelBufferGetHeight(self.0))?;
        if base.is_null() {
            return None;
        }
        // SAFETY: while locked, the buffer's base address starts `height` readable rows of
        // `bytesPerRow` bytes, and the slice cannot outlive the lock it borrows.
        Some(unsafe { slice::from_raw_parts(base, length) })
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
    let state = CGEventSourceStateID::CombinedSessionState;
    let flags = CGEventSource::flags_state(state);
    // Fn is Speakeasy's hold-to-talk key, so it may be down while Shift draws.
    let chords =
        CGEventFlags::MaskControl | CGEventFlags::MaskAlternate | CGEventFlags::MaskCommand;
    Some(PointerSample {
        at,
        x,
        y,
        drawing: flags.contains(CGEventFlags::MaskShift) && !flags.intersects(chords),
        pressed: CGEventSource::button_state(state, CGMouseButton::Left),
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
    let rect = CGDisplayBounds(id);
    Bounds {
        origin: [rect.origin.x, rect.origin.y],
        size: [rect.size.width, rect.size.height],
    }
}

/// Resizing calls back into GPUI, so call with GPUI's borrows released.
pub(crate) fn configure_overlay(handle: RawWindowHandle, display: Display) -> anyhow::Result<()> {
    let RawWindowHandle::AppKit(view) = handle else {
        bail!("Expected an AppKit window");
    };
    // SAFETY: GPUI supplies its live NSView on the main thread.
    let Some(window) = (unsafe { appkit_window(view) }) else {
        bail!("The overlay has no native window");
    };
    window.setIgnoresMouseEvents(true);
    window.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
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

pub(crate) fn set_overlay_visible(handle: RawWindowHandle, visible: bool) {
    let RawWindowHandle::AppKit(view) = handle else {
        return;
    };
    // SAFETY: GPUI supplies its live NSView on the main thread.
    let Some(window) = (unsafe { appkit_window(view) }) else {
        return;
    };
    if visible {
        window.orderFrontRegardless();
    } else {
        window.orderOut(None);
    }
}
