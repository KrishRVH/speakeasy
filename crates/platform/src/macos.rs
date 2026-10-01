//! macOS adapter: the Fn event tap, session-change cancellation, native window presentation, and
//! clipboard or direct text insertion.
//!
//! The tap's run-loop thread creates and removes every native resource its callbacks reach.

use std::{
    cell::RefCell,
    ffi::c_void,
    panic::{self, AssertUnwindSafe},
    ptr::NonNull,
    sync::Arc,
};

use anyhow::{anyhow, bail};
use block2::RcBlock;
use core_foundation::{
    base::{TCFType, kCFAllocatorDefault},
    runloop::{
        CFRunLoop, CFRunLoopActivity, CFRunLoopObserver, CFRunLoopObserverContext,
        CFRunLoopObserverCreate, CFRunLoopObserverRef, kCFRunLoopCommonModes, kCFRunLoopEntry,
    },
};
use core_graphics::{
    event::{
        CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventType, CallbackResult, EventField, KeyCode,
    },
    event_source::{CGEventSource, CGEventSourceStateID},
};
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSAlert, NSEvent, NSFloatingWindowLevel, NSPasteboard, NSScreen, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
    NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{
    NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSObjectProtocol,
    NSPoint, NSString,
};
use raw_window_handle::{AppKitWindowHandle, RawWindowHandle};

use super::{
    CANCEL_SHORTCUT, Delivery, Input, InputSender, InsertPermit, Inserted, OwnedThread,
    PILL_MARGIN, PILL_WIDTH, SHORTCUT, insertion,
    keyboard::{self, MacEvent},
    monitor::MonitorControl,
};

/// Tags input Speakeasy synthesizes, so its own event tap ignores it.
const OWN_INPUT: i64 = 0x5350_4541;
/// Core Graphics bounds a keyboard event's Unicode payload, so direct text posts in runs of about
/// this many UTF-16 units.
const TEXT_EVENT_UNITS: usize = 16;

/// Owns the desktop observation thread until explicit stop and acknowledged cleanup.
pub struct InputMonitor {
    // Declared first so dropping joins the thread before the lifecycle observers are removed.
    thread: OwnedThread,
    // Shared with the run-loop thread, which publishes its run loop for the owner to stop.
    control: Arc<MonitorControl<CFRunLoop>>,
    _observers: Vec<Observer>,
}

impl InputMonitor {
    pub(super) fn start(input: InputSender) -> anyhow::Result<Self> {
        let observers = lifecycle_observers(&input);
        let control = Arc::new(MonitorControl::default());
        let thread = OwnedThread::spawn("input-monitor", {
            let control = Arc::clone(&control);
            move || monitor(&input, &control)
        })?;
        Ok(Self {
            thread,
            control,
            _observers: observers,
        })
    }

    /// Requests native observation shutdown without joining its thread.
    pub fn request_stop(&self) {
        self.control.request_stop(CFRunLoop::stop);
    }

    /// Resolves once native resources have retired; await it before replacing or dropping the
    /// monitor.
    pub fn stopped(&self) -> impl Future<Output = ()> + use<> {
        self.thread.exited()
    }
}

impl Drop for InputMonitor {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// The run-loop entry observer's context, borrowed from `run_event_loop`'s frame.
struct RunLoopReady<'a> {
    control: &'a MonitorControl<CFRunLoop>,
    input: &'a InputSender,
}

impl RunLoopReady<'_> {
    /// Publishes the current run loop and reports readiness, or stops the run loop when a stop
    /// already arrived.
    fn publish(&self) {
        let run_loop = CFRunLoop::get_current();
        let started = self.control.start(run_loop.clone(), || {
            self.input.deliver(Input::DesktopReady {
                shortcut: SHORTCUT.into(),
                cancel: CANCEL_SHORTCUT.into(),
            });
        });
        if !started {
            run_loop.stop();
        }
    }
}

/// Removes a notification observer when dropped.
struct Observer {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Observer {
    fn cancel_on(
        center: Retained<NSNotificationCenter>,
        name: &NSString,
        input: &InputSender,
    ) -> Self {
        let input = input.clone();
        let callback = RcBlock::new(move |_: NonNull<NSNotification>| input.deliver(Input::Cancel));
        // SAFETY: the block captures a Send channel/atomic gate, accesses no AppKit objects, and
        // the center retains it until the observer is removed.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &callback)
        };
        Self { center, token }
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        // SAFETY: this token came from this center and stays retained until removal.
        unsafe {
            self.center.removeObserver((*self.token).as_ref());
        }
    }
}

fn lifecycle_observers(input: &InputSender) -> Vec<Observer> {
    let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: AppKit's exported notification names are immutable constants.
    let (will_sleep, resigned) = unsafe {
        (
            NSWorkspaceWillSleepNotification,
            NSWorkspaceSessionDidResignActiveNotification,
        )
    };
    let distributed = NSDistributedNotificationCenter::defaultCenter().into_super();
    let screen_locked = NSString::from_str("com.apple.screenIsLocked");
    vec![
        Observer::cancel_on(workspace.clone(), will_sleep, input),
        Observer::cancel_on(workspace, resigned, input),
        Observer::cancel_on(distributed, &screen_locked, input),
    ]
}

fn monitor(input: &InputSender, control: &MonitorControl<CFRunLoop>) {
    if let Err(error) = run_event_loop(input, control)
        && !control.stopping()
        && !input.is_closed()
    {
        input.deliver(Input::Unavailable(error.to_string()));
    }
    control.retire();
    input.close();
}

fn run_event_loop(input: &InputSender, control: &MonitorControl<CFRunLoop>) -> anyhow::Result<()> {
    if control.stopping() {
        return Ok(());
    }
    let tap = shortcut_tap(input)?;
    let source = tap.mach_port().create_runloop_source(0).map_err(|()| {
        anyhow!("Cannot create shortcut run loop. Pause and resume dictation to try again.")
    })?;
    let run_loop = CFRunLoop::get_current();
    let mut ready = RunLoopReady { control, input };
    let mut context = CFRunLoopObserverContext {
        version: 0,
        info: (&raw mut ready).cast(),
        retain: None,
        release: None,
        copyDescription: None,
    };
    // SAFETY: this thread owns the observer and its stack context throughout CFRunLoopRun. Entry
    // fires once, and the observer is removed before return.
    let observer = unsafe {
        let raw = CFRunLoopObserverCreate(
            kCFAllocatorDefault,
            kCFRunLoopEntry,
            0,
            0,
            on_run_loop_entry,
            &raw mut context,
        );
        if raw.is_null() {
            bail!("Cannot prepare shortcut monitoring. Pause and resume dictation to try again.");
        }
        CFRunLoopObserver::wrap_under_create_rule(raw)
    };
    // SAFETY: this is Core Foundation's permanent mode constant.
    let modes = unsafe { kCFRunLoopCommonModes };
    run_loop.add_source(&source, modes);
    run_loop.add_observer(&observer, modes);
    tap.enable();
    CFRunLoop::run_current();
    run_loop.remove_observer(&observer, modes);
    run_loop.remove_source(&source, modes);
    Ok(())
}

fn shortcut_tap(input: &InputSender) -> anyhow::Result<CGEventTap<'static>> {
    #[expect(
        clippy::disallowed_types,
        reason = "CGEventTap requires an Fn callback; this policy belongs exclusively to that callback on its native run-loop thread"
    )]
    let policy = RefCell::new(keyboard::Mac::default());
    let input = input.clone();
    CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ],
        move |_, kind, event| {
            // A panic must not unwind into the OS. Closing the input makes the owner cancel rather
            // than continue with lost key edges.
            panic::catch_unwind(AssertUnwindSafe(|| {
                observe(&input, &mut policy.borrow_mut(), kind, event)
            }))
            .unwrap_or_else(|_| {
                input.close();
                CallbackResult::Keep
            })
        },
    )
    .map_err(|()| {
        anyhow!(
            "Allow Speakeasy in System Settings → Privacy & Security → Accessibility, then pause and resume dictation."
        )
    })
}

/// Publishes the run loop only once it has entered: `CFRunLoopStop` before entry does not reliably
/// stop a later run, so a stop that arrived earlier ends the run from here.
extern "C" fn on_run_loop_entry(
    _: CFRunLoopObserverRef,
    _: CFRunLoopActivity,
    context: *mut c_void,
) {
    // SAFETY: run_event_loop keeps this stack context alive until the run loop has returned and
    // removes its observer before releasing the context.
    let ready = unsafe { &*context.cast::<RunLoopReady<'_>>() };
    if panic::catch_unwind(AssertUnwindSafe(|| ready.publish())).is_err() {
        ready.input.close();
        CFRunLoop::get_current().stop();
    }
}

/// Fn, Fn+Space, and passive Escape, on the event tap's run-loop thread.
fn observe(
    input: &InputSender,
    policy: &mut keyboard::Mac,
    kind: CGEventType,
    event: &CGEvent,
) -> CallbackResult {
    if matches!(
        kind,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        // macOS dropped key events while the tap was disabled; closing makes the owner cancel
        // instead of trusting lost key edges.
        input.close();
        return CallbackResult::Keep;
    }
    if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == OWN_INPUT {
        return CallbackResult::Keep;
    }
    let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
    let key_event = match kind {
        CGEventType::KeyDown => MacEvent::Key { code, down: true },
        CGEventType::KeyUp => MacEvent::Key { code, down: false },
        CGEventType::FlagsChanged => MacEvent::FlagsChanged {
            code,
            function: event
                .get_flags()
                .contains(CGEventFlags::CGEventFlagSecondaryFn),
        },
        _ => return CallbackResult::Keep,
    };
    let decision = policy.observe(key_event);
    decision.deliver(input);
    if decision.swallows() {
        CallbackResult::Drop
    } else {
        CallbackResult::Keep
    }
}

/// Configures an owned UI-thread window for nonactivating, click-through presentation.
///
/// # Errors
/// Returns an error for the wrong window kind or failed native configuration.
pub fn configure_pill(handle: RawWindowHandle) -> anyhow::Result<()> {
    let RawWindowHandle::AppKit(view) = handle else {
        bail!("Expected an AppKit window");
    };
    // SAFETY: GPUI supplies its live NSView on the main thread.
    let Some(window) = (unsafe { appkit_window(view) }) else {
        bail!("The pill has no native window");
    };
    window.setIgnoresMouseEvents(true);
    // A titled panel highlights its top edge in dark mode and shadows the whole transparent window;
    // the capsule draws its own shadow. The view already fills the frame, so its size holds.
    window.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
    window.setHasShadow(false);
    window.setLevel(NSFloatingWindowLevel);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    Ok(())
}

/// Shows or hides the owned Settings window; call on its UI thread with GPUI's borrows released,
/// because `AppKit` can call back into GPUI.
pub fn set_settings_visible(handle: RawWindowHandle, visible: bool) {
    let RawWindowHandle::AppKit(view) = handle else {
        return;
    };
    // SAFETY: GPUI supplies its live NSView on the main thread.
    let Some(window) = (unsafe { appkit_window(view) }) else {
        return;
    };
    if visible {
        window.deminiaturize(None);
        window.makeKeyAndOrderFront(None);
    } else {
        window.orderOut(None);
    }
}

/// Shows or hides the owned pill without activating it; call on its UI thread.
pub fn set_pill_visible(handle: RawWindowHandle, visible: bool) {
    let RawWindowHandle::AppKit(view) = handle else {
        return;
    };
    // SAFETY: GPUI supplies its live NSView on the main thread.
    let Some(window) = (unsafe { appkit_window(view) }) else {
        return;
    };
    if visible {
        if let Some(origin) = pill_origin_under_pointer() {
            window.setFrameOrigin(origin);
        }
        window.orderFrontRegardless();
    } else {
        window.orderOut(None);
    }
}

/// Bottom-center of the visible frame of the screen under the pointer.
fn pill_origin_under_pointer() -> Option<NSPoint> {
    let main = MainThreadMarker::new()?;
    let pointer = NSEvent::mouseLocation();
    let screen = NSScreen::screens(main).iter().find(|screen| {
        let frame = screen.frame();
        pointer.x >= frame.origin.x
            && pointer.x < frame.origin.x + frame.size.width
            && pointer.y >= frame.origin.y
            && pointer.y < frame.origin.y + frame.size.height
    })?;
    let frame = screen.visibleFrame();
    Some(NSPoint::new(
        frame.origin.x + (frame.size.width - f64::from(PILL_WIDTH)) / 2.0,
        frame.origin.y + f64::from(PILL_MARGIN),
    ))
}

/// The window hosting `view`, if it has one.
///
/// # Safety
/// `view` must name a live `NSView`, used on the main thread.
unsafe fn appkit_window(view: AppKitWindowHandle) -> Option<Retained<NSWindow>> {
    // SAFETY: the caller passes a live NSView on the main thread, which AppKit requires of it.
    unsafe { view.ns_view.cast::<NSView>().as_ref() }.window()
}

pub(super) fn modifiers_down() -> bool {
    CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .and_then(CGEvent::new)
        // An unreadable modifier state counts as held, so insertion waits rather than pasting early.
        .map_or(true, |event| {
            event.get_flags().intersects(
                CGEventFlags::CGEventFlagControl
                    | CGEventFlags::CGEventFlagAlternate
                    | CGEventFlags::CGEventFlagCommand
                    | CGEventFlags::CGEventFlagShift
                    | CGEventFlags::CGEventFlagSecondaryFn,
            )
        })
}

pub(super) fn insert(
    text: &str,
    permit: &InsertPermit,
    delivery: Delivery,
) -> anyhow::Result<Inserted> {
    if !permit.active() {
        return Ok(Inserted::Cancelled);
    }
    match delivery {
        Delivery::Paste => paste(text, permit),
        Delivery::Direct => insert_direct(text, permit),
    }
}

fn paste(text: &str, permit: &InsertPermit) -> anyhow::Result<Inserted> {
    let mut clipboard = arboard::Clipboard::new()?;
    if !permit.active() {
        return Ok(Inserted::Cancelled);
    }
    clipboard.set_text(text)?;
    let sequence = clipboard_sequence();
    if clipboard.get_text()? != text || clipboard_sequence() != sequence {
        return Ok(Inserted::Unavailable(
            "Clipboard changed. New contents were preserved; paste was not sent.",
        ));
    }
    let source = private_event_source()?;
    let mut events = Vec::with_capacity(2);
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(source.clone(), KeyCode::ANSI_V, down)
            .map_err(|()| anyhow!("Cannot create paste event"))?;
        event.set_flags(CGEventFlags::CGEventFlagCommand);
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
        events.push(event);
    }
    if let Some(outcome) = Delivery::Paste.preflight(has_external_target(), !modifiers_down()) {
        return Ok(outcome);
    }
    if clipboard_sequence() != sequence {
        return Ok(Inserted::Unavailable(
            "Clipboard changed before paste. New contents were preserved.",
        ));
    }
    Ok(commit_and_post(events, permit))
}

fn insert_direct(text: &str, permit: &InsertPermit) -> anyhow::Result<Inserted> {
    let source = private_event_source()?;
    let mut events = Vec::new();
    for chunk in insertion::utf16_chunks(text, TEXT_EVENT_UNITS) {
        let press = CGEvent::new_keyboard_event(source.clone(), 0, true)
            .map_err(|()| anyhow!("Cannot create text event"))?;
        press.set_string(chunk);
        let release = CGEvent::new_keyboard_event(source.clone(), 0, false)
            .map_err(|()| anyhow!("Cannot create text release"))?;
        for event in [press, release] {
            event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
            events.push(event);
        }
    }
    if let Some(outcome) = Delivery::Direct.preflight(has_external_target(), !modifiers_down()) {
        return Ok(outcome);
    }
    Ok(commit_and_post(events, permit))
}

/// Posts `events` only if `permit` commits. Callers create every event first, so a failed creation
/// cannot leave partial input posted.
fn commit_and_post(events: Vec<CGEvent>, permit: &InsertPermit) -> Inserted {
    if !permit.commit() {
        return Inserted::Cancelled;
    }
    for event in events {
        event.post(CGEventTapLocation::HID);
    }
    Inserted::Sent
}

fn private_event_source() -> anyhow::Result<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|()| anyhow!("Cannot create keyboard event source"))
}

fn clipboard_sequence() -> isize {
    NSPasteboard::generalPasteboard().changeCount()
}

/// Returns whether the system asks apps to reduce motion; Linux uses the app setting.
#[must_use]
pub fn reduced_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// Presents a local startup error; callers never include audio, transcripts, or credentials.
pub fn show_error(message: &str) {
    if let Some(main) = MainThreadMarker::new() {
        let alert = NSAlert::new(main);
        alert.setMessageText(&NSString::from_str("Speakeasy could not start"));
        alert.setInformativeText(&NSString::from_str(message));
        alert.runModal();
    }
}

fn has_external_target() -> bool {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .is_some_and(|app| i64::from(app.processIdentifier()) != i64::from(std::process::id()))
}
