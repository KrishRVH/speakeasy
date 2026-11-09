mod stream;

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
    kCGDockWindowLevel, kCGMainMenuWindowLevel, kCGNormalWindowLevel, kCGNullDirectDisplay,
    kCGNullWindowID, kCGPopUpMenuWindowLevel, kCGUtilityWindowLevel, kCGWindowAlpha,
    kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerPID,
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
        self, Frame, Listed, Part, PointerSample, Region, ScreenEvent, Target, WindowGone,
        WindowLayer,
        geometry::{Bounds, fit, narrow, pack_rows, pixels},
    },
};

const NO_POINTER: &str = "Cannot find the pointer. Try again.";

const PATIENCE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
enum Failure {
    Window(WindowGone),
    Permission,
    Stalled,
    Unstarted,
    DisplayLost,
    Exposed,
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
        let message = match self {
            Self::Window(gone) => return gone.fmt(f),
            Self::Permission => {
                "Allow Screen & System Audio Recording for Speakeasy in System Settings › Privacy & Security. If it is already on, remove it with − and add it again, then start the Show again."
            },
            Self::Stalled => "Screen capture stopped responding. Start the Show again.",
            Self::Unstarted => "Cannot start screen capture. Start the Show again.",
            Self::DisplayLost => {
                "The recorded display is no longer available. Start the Show again."
            },
            Self::Exposed => {
                "Cannot keep Speakeasy's windows out of the Show. Start the Show again."
            },
        };
        f.write_str(message)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Display {
    id: CGDirectDisplayID,
    bounds: Bounds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window(CGWindowID);

struct Described {
    number: CGWindowID,
    owner: i64,
    layer: i64,
    alpha: f64,
    on_screen: bool,
    bounds: Bounds,
}

pub(crate) fn window_under_pointer(display: Display) -> anyhow::Result<Option<(Window, Part)>> {
    let pointer = pointer_location().context(NO_POINTER)?;
    let options =
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let listed = windows(options, kCGNullWindowID).context("Cannot list windows. Try again.")?;
    Ok(listed
        .into_iter()
        .find(|window| window.layer == 0 && window.alpha > 0.0 && window.bounds.contains(pointer))
        .filter(|window| !super::is_own_process(window.owner))
        .and_then(|front| {
            let part = display.bounds.part_covered_by(front.bounds)?;
            Some((Window(front.number), part))
        }))
}

pub(crate) fn stack(display: Display) -> Option<Vec<Listed>> {
    let options =
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let listed = windows(options, kCGNullWindowID)?;
    Some(
        listed
            .into_iter()
            .filter(|window| window.alpha > 0.0 && !super::is_own_process(window.owner))
            .filter_map(|window| {
                Some(Listed {
                    window: u64::from(window.number),
                    process: u64::try_from(window.owner).ok()?,
                    layer: layer(window.layer)?,
                    part: display.bounds.part_covered_by(window.bounds)?,
                })
            })
            .collect(),
    )
}

fn layer(level: i64) -> Option<WindowLayer> {
    let level = i32::try_from(level).ok()?;
    if level == kCGNormalWindowLevel {
        Some(WindowLayer::Normal)
    } else if (kCGNormalWindowLevel..=kCGUtilityWindowLevel).contains(&level) {
        Some(WindowLayer::Floating)
    } else if level == kCGPopUpMenuWindowLevel {
        Some(WindowLayer::Menu)
    } else if level == kCGDockWindowLevel || level == kCGMainMenuWindowLevel {
        Some(WindowLayer::Bar)
    } else {
        None
    }
}

enum Place {
    Covers(Part),
    Unknown,
}

fn place(target: Target) -> Result<Place, WindowGone> {
    let Region::Window(window, _) = target.region else {
        return Ok(Place::Covers(target.part()));
    };
    let Some(listed) = windows(CGWindowListOption::OptionIncludingWindow, window.0) else {
        return Ok(Place::Unknown);
    };
    let described = listed
        .into_iter()
        .find(|described| described.number == window.0)
        .ok_or(WindowGone::Closed)?;
    if !described.on_screen {
        return Err(WindowGone::OffScreen);
    }
    target
        .display()
        .native
        .bounds
        .part_covered_by(described.bounds)
        .map(Place::Covers)
        .ok_or(WindowGone::OffScreen)
}

fn windows(options: CGWindowListOption, window: CGWindowID) -> Option<Vec<Described>> {
    let list = CGWindowListCopyWindowInfo(options, window)?;

    let list =
        unsafe { CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFType>>>(list) };
    Some(list.iter().filter_map(|window| describe(&window)).collect())
}

fn describe(window: &CFDictionary<CFString, CFType>) -> Option<Described> {
    let [number, owner, layer, alpha, on_screen, bounds] = unsafe {
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

    if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(&bounds), &raw mut rect) } {
        return None;
    }
    Some(Described {
        number: u32::try_from(numeric(number)?.as_i64()?).ok()?,
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
    let [x, y] = pointer_location().context(NO_POINTER)?;
    let mut id = kCGNullDirectDisplay;
    let mut found = 0;

    let status =
        unsafe { CGGetDisplaysWithPoint(CGPoint::new(x, y), 1, &raw mut id, &raw mut found) };
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
    movie: Option<crate::MovieTarget>,
) {
    if matches!(target.region, Region::Window(_, _)) {
        drop(movie);
        capture_stills_owned(target, interval, longest_edge, events, stopping);
    } else if let Err(failure) =
        stream::capture(target, interval, longest_edge, events, stopping, movie)
    {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The final failure replaces a queued still; a closed lane has no observer"
        )]
        let _ = events.force_send(failure.into());
    }
}

fn capture_stills_owned(
    target: Target,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
    stopping: &Receiver<Infallible>,
) {
    let Ok(runtime) = runtime::Builder::new_current_thread().enable_time().build() else {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A closed lane means nobody is waiting for this capture"
        )]
        let _ = events.try_send(Failure::Unstarted.into());
        return;
    };
    runtime.block_on(async {
        let failed = async {
            let Err(failure) = capture_stills(target, interval, longest_edge, events).await;

            events.send(failure.into()).await
        };
        tokio::select! {
            _ = stopping.recv() => {},
            () = events.closed() => {},
            _ = failed => {},
        }
    });
}

async fn capture_stills(
    target: Target,
    interval: Duration,
    longest_edge: u32,
    events: &Sender<ScreenEvent>,
) -> Result<Infallible, Failure> {
    let source = answer(Source::fetch(target.display().native.id, longest_edge))
        .await
        .unwrap_or(Err(Failure::Stalled))?;
    let mut last_answer = Instant::now();
    loop {
        let still = match place(target)? {
            Place::Covers(part) => answer(source.still(part)?).await.unwrap_or(Ok(None))?,
            Place::Unknown => None,
        };
        if let Some(frame) = still
            && let Place::Covers(part) = place(target)?
        {
            last_answer = frame.at;

            if part == frame.part {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A frame the lane has no room for is dropped, and a closed lane ends capture"
                )]
                let _ = events.try_send(ScreenEvent::Frame(frame));
            }
        }
        if last_answer.elapsed() >= PATIENCE {
            return Err(Failure::Stalled);
        }

        time::sleep(interval).await;
    }
}

struct Source {
    filter: Retained<SCContentFilter>,
    points: [f64; 2],
    scale: f64,
    longest_edge: u32,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "ScreenCaptureKit content filters are immutable, retained, and have no thread affinity"
)]
unsafe impl Send for Source {}

impl Source {
    fn fetch(id: CGDirectDisplayID, longest_edge: u32) -> Receiver<Result<Self, Failure>> {
        ask(
            |handler| {
                unsafe { SCShareableContent::getShareableContentWithCompletionHandler(handler) };
            },
            move |content: Option<&SCShareableContent>, _: Option<&NSError>| {
                Self::new(content.ok_or(Failure::Permission)?, id, longest_edge)
            },
        )
    }

    fn new(
        content: &SCShareableContent,
        id: CGDirectDisplayID,
        longest_edge: u32,
    ) -> Result<Self, Failure> {
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

        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::from_retained_slice(&[own]),
                &NSArray::new(),
            )
        };

        let (points, scale) = unsafe { (filter.contentRect().size, filter.pointPixelScale()) };
        Ok(Self {
            filter,
            points: [points.width, points.height],
            scale: f64::from(scale),
            longest_edge,
        })
    }

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

        let configuration = unsafe {
            let configuration = SCStreamConfiguration::new();

            configuration.setSourceRect(area);
            configuration.setWidth(width as usize);
            configuration.setHeight(height as usize);
            configuration.setPixelFormat(kCVPixelFormatType_32BGRA);

            configuration.setColorSpaceName(kCGColorSpaceSRGB);

            configuration.setShowsCursor(false);

            configuration.setPreservesAspectRatio(false);
            configuration
        };
        Some(configuration)
    }

    fn still(&self, part: Part) -> Result<Receiver<Result<Option<Frame>, Failure>>, Failure> {
        let configuration = self.configuration(part).ok_or(Failure::DisplayLost)?;
        Ok(ask(
            |handler| {
                unsafe {
                    SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
                        &self.filter,
                        &configuration,
                        Some(handler),
                    );
                };
            },
            move |sample: Option<&CMSampleBuffer>, error: Option<&NSError>| match sample {
                Some(sample) => Ok(frame(sample, part, Instant::now())),
                None if error.is_some_and(refused) => Err(Failure::Permission),
                None => Ok(None),
            },
        ))
    }
}

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
        let (value, error) = unsafe { (value.as_ref(), error.as_ref()) };

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

async fn answer<O>(replies: Receiver<O>) -> Option<O> {
    time::timeout(PATIENCE, replies.recv()).await.ok()?.ok()
}

fn frame(sample: &CMSampleBuffer, part: Part, captured: Instant) -> Option<Frame> {
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
        at: captured,
        width: width.try_into().ok()?,
        height: height.try_into().ok()?,
        bgra,
        part,
    })
}

struct Locked<'a>(&'a CVPixelBuffer);

impl<'a> Locked<'a> {
    fn new(buffer: &'a CVPixelBuffer) -> Option<Self> {
        let status =
            unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
        (status == kCVReturnSuccess).then(|| Self(buffer))
    }

    fn rows(&self) -> Option<&[u8]> {
        let base = NonNull::new(CVPixelBufferGetBaseAddress(self.0).cast::<u8>())?;
        let length =
            CVPixelBufferGetBytesPerRow(self.0).checked_mul(CVPixelBufferGetHeight(self.0))?;

        Some(unsafe { slice::from_raw_parts(base.as_ptr(), length) })
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly) };
    }
}

fn refused(error: &NSError) -> bool {
    let domain = unsafe { SCStreamErrorDomain };
    *error.domain() == *domain && error.code() == SCStreamErrorCode::UserDeclined.0
}

pub(crate) fn pointer(target: Target, shortcut: crate::MacShortcut) -> Option<PointerSample> {
    let Place::Covers(part) = place(target).ok()? else {
        return None;
    };
    let at = Instant::now();
    Some(PointerSample::Observed {
        at,
        position: target
            .display
            .native
            .bounds
            .normalize(pointer_location()?)?,
        part,
        drawing: super::modifiers_for(shortcut).draws(),
        pressed: CGEventSource::button_state(
            CGEventSourceStateID::CombinedSessionState,
            CGMouseButton::Left,
        ),
    })
}

fn pointer_location() -> Option<[f64; 2]> {
    let event = CGEvent::new(None)?;
    let point = CGEvent::location(Some(&event));
    Some([point.x, point.y])
}

fn display_bounds(id: CGDirectDisplayID) -> Bounds {
    Bounds::from(CGDisplayBounds(id))
}

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
    reason = "Overlay configuration can fail on Windows and Linux; callers share one signature on every platform"
)]
pub(crate) fn configure_overlay(window: &NativeWindow, display: Display) -> anyhow::Result<()> {
    let window = &window.native.ns_window;
    window.setIgnoresMouseEvents(true);
    window.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);

    if let Some(panel) = window.downcast_ref::<NSPanel>() {
        panel.setBecomesKeyOnlyIfNeeded(true);
    }
    window.setHasShadow(false);

    window.setSharingType(NSWindowSharingType::None);

    window.setLevel(NSStatusWindowLevel.saturating_add(1));
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );

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

#[cfg(test)]
mod tests {
    use objc2_core_graphics::{
        kCGDesktopWindowLevel, kCGFloatingWindowLevel, kCGModalPanelWindowLevel,
        kCGScreenSaverWindowLevel,
    };

    use super::*;

    #[test]
    fn window_levels_sort_into_ordinary_floating_menu_and_bar_layers() {
        let sorted = |level: i32| layer(i64::from(level));
        assert_eq!(sorted(kCGNormalWindowLevel), Some(WindowLayer::Normal));
        assert_eq!(sorted(kCGFloatingWindowLevel), Some(WindowLayer::Floating));
        assert_eq!(
            sorted(kCGModalPanelWindowLevel),
            Some(WindowLayer::Floating)
        );
        assert_eq!(sorted(kCGUtilityWindowLevel), Some(WindowLayer::Floating));
        assert_eq!(sorted(kCGPopUpMenuWindowLevel), Some(WindowLayer::Menu));
        assert_eq!(sorted(kCGMainMenuWindowLevel), Some(WindowLayer::Bar));
        assert_eq!(sorted(kCGDockWindowLevel), Some(WindowLayer::Bar));
        assert_eq!(sorted(kCGDesktopWindowLevel), None);
        assert_eq!(sorted(kCGScreenSaverWindowLevel), None);
    }
}
