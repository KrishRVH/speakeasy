//! macOS adapter: the Fn event tap, session-change interruption, native window presentation,
//! clipboard or direct text insertion, and Show screen capture.
//!
//! The tap's run-loop thread creates and removes every native resource its callbacks reach.

pub(crate) mod screen;

use std::{
    cell::OnceCell,
    ffi::c_void,
    panic::{self, AssertUnwindSafe},
    ptr::NonNull,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Instant,
};

use anyhow::{Context as _, anyhow, bail, ensure};
use block2::RcBlock;
use core_foundation::{
    base::{TCFType, kCFAllocatorDefault},
    mach_port::CFMachPort,
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
    NSAlert, NSEvent, NSFloatingWindowLevel, NSPasteboard, NSPasteboardContentsOptions,
    NSPasteboardTypeString, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask, NSWorkspace, NSWorkspaceSessionDidResignActiveNotification,
    NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{
    NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSObjectProtocol,
    NSPoint, NSString,
};
use raw_window_handle::{RawWindowHandle, WindowHandle};

use super::{
    CANCEL_SHORTCUT, DesktopOptions, Input, InputSender, Inserted, Inserter, NativeWindow,
    OwnedThread, PILL_MARGIN, PILL_WIDTH, SHORTCUT, insertion,
    keyboard::{self, MacEvent, Modifiers},
    monitor::MonitorControl,
};

/// Tags input Speakeasy synthesizes, so its own event tap ignores it.
const OWN_INPUT: i64 = 0x5350_4541;
/// Core Graphics bounds a keyboard event's Unicode payload, so direct text posts in runs of about
/// this many UTF-16 units.
const TEXT_EVENT_UNITS: usize = 16;

/// What `InputMonitor` holds beside the event tap's thread, which it joins before dropping this:
/// the tap's stop control, the lifecycle observers, and the insertion thread.
pub(super) struct Monitor {
    // Shared with the run-loop thread, which publishes its run loop for the owner to stop.
    control: Arc<MonitorControl<CFRunLoop>>,
    _observers: Vec<Observer>,
    insertion: insertion::Worker,
}

impl Monitor {
    pub(super) fn request_stop(&self) {
        self.control.request_stop(CFRunLoop::stop);
        self.insertion.request_stop();
    }

    pub(super) fn stopped(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        self.insertion.stopped()
    }
}

pub(super) fn start(
    input: InputSender,
    _: DesktopOptions,
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    let (insertion, inserter) = insertion::Worker::start(input.clone())?;
    let policy = Arc::new(SharedPolicy::default());
    let observers = lifecycle_observers(&input, &policy);
    let control = Arc::new(MonitorControl::default());
    let thread = OwnedThread::spawn("input-monitor", {
        let control = Arc::clone(&control);
        move || monitor(&input, &control, &policy)
    })?;
    let monitor = Monitor {
        control,
        _observers: observers,
        insertion,
    };
    Ok((thread, monitor, inserter))
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

/// The Fn policy, which the event tap decides on its run-loop thread and lifecycle notifications
/// interrupt from the thread that posts them.
#[derive(Default)]
struct SharedPolicy {
    #[expect(
        clippy::disallowed_types,
        reason = "AppKit posts lifecycle notifications on its own thread while the event tap decides keys on its run loop, which cannot receive them as events; deciding and delivering under one lock keeps the owner's inputs in decision order"
    )]
    policy: Mutex<keyboard::Mac>,
}

impl SharedPolicy {
    /// A panic while deciding already closed the input lane, so a poisoned policy decides nothing
    /// the owner still hears.
    fn lock(&self) -> MutexGuard<'_, keyboard::Mac> {
        self.policy.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Removes a notification observer when dropped.
struct Observer {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Observer {
    fn interrupt_on(
        center: Retained<NSNotificationCenter>,
        name: &NSString,
        input: &InputSender,
        policy: &Arc<SharedPolicy>,
    ) -> Self {
        let input = input.clone();
        let policy = Arc::clone(policy);
        let callback = RcBlock::new(move |_: NonNull<NSNotification>| {
            // A panic must not unwind into AppKit.
            let interrupted =
                panic::catch_unwind(AssertUnwindSafe(|| policy.lock().interrupt(&input)));
            if interrupted.is_err() {
                input.close();
            }
        });
        // SAFETY: with no queue the block runs on whichever thread posts the notification; it
        // captures only an `InputSender` and the locked policy, which any thread may use, touches
        // no AppKit object, and the center retains it until the observer is removed.
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

fn lifecycle_observers(input: &InputSender, policy: &Arc<SharedPolicy>) -> Vec<Observer> {
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
        Observer::interrupt_on(workspace.clone(), will_sleep, input, policy),
        Observer::interrupt_on(workspace, resigned, input, policy),
        Observer::interrupt_on(distributed, &screen_locked, input, policy),
    ]
}

fn monitor(input: &InputSender, control: &MonitorControl<CFRunLoop>, policy: &SharedPolicy) {
    if let Err(error) = run_event_loop(input, control, policy)
        && !control.stopping()
        && !input.is_closed()
    {
        input.deliver(Input::Unavailable(error.to_string()));
    }
    control.retire();
    input.close();
}

fn run_event_loop(
    input: &InputSender,
    control: &MonitorControl<CFRunLoop>,
    policy: &SharedPolicy,
) -> anyhow::Result<()> {
    if control.stopping() {
        return Ok(());
    }
    let port = TapPort::default();
    let tap = shortcut_tap(input, policy, &port)?;
    port.publish(tap.mach_port());
    let source = tap.mach_port().create_runloop_source(0).map_err(|()| {
        anyhow!("Cannot watch the dictation shortcut. Resume dictation to try again.")
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
            bail!("Cannot watch the dictation shortcut. Resume dictation to try again.");
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

/// The event tap's port, which the tap's own callback needs to turn the tap back on.
#[derive(Default)]
struct TapPort {
    #[expect(
        clippy::disallowed_types,
        reason = "macOS returns the tap's port only once the callback that needs it is registered"
    )]
    port: OnceCell<CFMachPort>,
}

impl TapPort {
    /// Records the port before the run loop can deliver any event to the tap.
    fn publish(&self, port: &CFMachPort) {
        self.port.get_or_init(|| port.clone());
    }

    /// Turns the tap back on after macOS turned it off; returns whether it is on.
    fn reenable(&self) -> bool {
        let Some(port) = self.port.get() else {
            return false;
        };
        // SAFETY: both crates name the same Core Foundation port, which `self` keeps alive
        // throughout this borrow.
        let port = unsafe {
            &*port
                .as_concrete_TypeRef()
                .cast::<objc2_core_foundation::CFMachPort>()
        };
        objc2_core_graphics::CGEvent::tap_enable(port, true);
        objc2_core_graphics::CGEvent::tap_is_enabled(port)
    }
}

fn shortcut_tap<'tap>(
    input: &'tap InputSender,
    policy: &'tap SharedPolicy,
    port: &'tap TapPort,
) -> anyhow::Result<CGEventTap<'tap>> {
    // SAFETY: run_event_loop drops the tap before the state it borrows and adds the tap's source
    // only to this thread's run loop, so the callback runs only here while that state lives.
    unsafe {
        CGEventTap::new_unchecked(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            vec![
                CGEventType::KeyDown,
                CGEventType::KeyUp,
                CGEventType::FlagsChanged,
            ],
            move |_, kind, event| {
                // A panic must not unwind into the OS. Closing the input makes the owner cancel
                // rather than continue with lost key edges.
                panic::catch_unwind(AssertUnwindSafe(|| {
                    observe(input, &mut policy.lock(), port, kind, event)
                }))
                .unwrap_or_else(|_| {
                    input.close();
                    CallbackResult::Keep
                })
            },
        )
    }
    .map_err(|()| {
        anyhow!(
            "Allow Speakeasy in System Settings › Privacy & Security › Accessibility. If it is already on, remove it with − and add it again, then resume dictation."
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

/// Fn, Fn+Space, Fn+S, W, or A, passive Escape, and Shift taps, on the event tap's run-loop
/// thread.
fn observe(
    input: &InputSender,
    policy: &mut keyboard::Mac,
    port: &TapPort,
    kind: CGEventType,
    event: &CGEvent,
) -> CallbackResult {
    if matches!(
        kind,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        // Key events went by unseen while the tap was off, so the owner cannot trust the hold's
        // edges: dictation ends and the chord waits for its release while the tap watches again.
        policy.interrupt(input);
        if !port.reenable() {
            input.close();
        }
        return CallbackResult::Keep;
    }
    if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == OWN_INPUT {
        return CallbackResult::Keep;
    }
    let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
    // Both crates mirror the same CGEventFlags bits.
    let held = held(objc2_core_graphics::CGEventFlags(event.get_flags().bits()));
    let key_event = match kind {
        CGEventType::KeyDown => MacEvent::Key {
            code,
            down: true,
            held,
        },
        CGEventType::KeyUp => MacEvent::Key {
            code,
            down: false,
            held,
        },
        CGEventType::FlagsChanged => MacEvent::FlagsChanged { code, held },
        _ => return CallbackResult::Keep,
    };
    let decision = policy.observe(key_event, Instant::now());
    decision.deliver(input);
    if decision.swallows() {
        CallbackResult::Drop
    } else {
        CallbackResult::Keep
    }
}

impl NativeWindow {
    /// Configures the pill for nonactivating, click-through presentation.
    ///
    /// # Errors
    /// `AppKit` configuration cannot fail; the `Result` matches the other platforms.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "Windows and X11 configuration can fail; callers share one signature on every platform"
    )]
    pub fn configure_pill(&self) -> anyhow::Result<()> {
        let window = &self.native.ns_window;
        window.setIgnoresMouseEvents(true);
        // A titled panel highlights its top edge in dark mode and shadows the whole transparent
        // window; the capsule draws its own shadow. The view already fills the frame, so its size
        // holds.
        window.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
        window.setHasShadow(false);
        window.setLevel(NSFloatingWindowLevel);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        Ok(())
    }

    /// Shows the Settings window in front, or hides it.
    pub fn set_settings_visible(&self, visible: bool) {
        let window = &self.native.ns_window;
        if visible {
            window.deminiaturize(None);
            window.makeKeyAndOrderFront(None);
        } else {
            window.orderOut(None);
        }
    }

    /// Shows or hides the pill without activating it.
    pub fn set_pill_visible(&self, visible: bool) {
        let window = &self.native.ns_window;
        if visible {
            if let Some(origin) = pill_origin_under_pointer() {
                window.setFrameOrigin(origin);
            }
            window.orderFrontRegardless();
        } else {
            window.orderOut(None);
        }
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

/// A GPUI window's `NSWindow`, retained so it stays valid. `NSWindow` is main-thread-only, so
/// neither it nor this is `Send`.
pub(super) struct Window {
    ns_window: Retained<NSWindow>,
}

impl Window {
    pub(super) fn new(handle: WindowHandle<'_>) -> anyhow::Result<Self> {
        ensure!(
            MainThreadMarker::new().is_some(),
            "AppKit windows are configured on the main thread"
        );
        let RawWindowHandle::AppKit(view) = handle.as_raw() else {
            bail!("Expected an AppKit window");
        };
        // SAFETY: the borrowed handle keeps its NSView live, and the marker proves this is the main
        // thread AppKit requires.
        let view = unsafe { view.ns_view.cast::<NSView>().as_ref() };
        let ns_window = view.window().context("The view has no window")?;
        Ok(Self { ns_window })
    }
}

/// The modifier keys held now, as the session's combined input state reports them.
pub(super) fn modifiers() -> Modifiers {
    use objc2_core_graphics::{CGEventSource, CGEventSourceStateID};

    held(CGEventSource::flags_state(
        CGEventSourceStateID::CombinedSessionState,
    ))
}

/// The modifier keys `flags` report held.
fn held(flags: objc2_core_graphics::CGEventFlags) -> Modifiers {
    use objc2_core_graphics::CGEventFlags;

    Modifiers {
        shift: flags.contains(CGEventFlags::MaskShift),
        shortcut: flags.intersects(
            CGEventFlags::MaskControl | CGEventFlags::MaskAlternate | CGEventFlags::MaskCommand,
        ),
        talk: flags.contains(CGEventFlags::MaskSecondaryFn),
    }
}

pub(super) fn copy_text(text: &str) -> anyhow::Result<()> {
    arboard::Clipboard::new()?.set_text(text)?;
    Ok(())
}

/// The native steps of one insertion, which [`insertion::insert`] puts in order.
pub(super) struct Insert;

impl insertion::NativeInsert for Insert {
    /// Every event is created before commit, so a failed creation cannot leave partial input.
    type Keys = Vec<CGEvent>;
    type Lease = Retained<NSPasteboard>;
    /// The pasteboard change count, which every change advances.
    type Mark = isize;

    fn acquire(&mut self) -> anyhow::Result<Retained<NSPasteboard>> {
        Ok(NSPasteboard::generalPasteboard())
    }

    /// Keeps the text on this Mac, out of Universal Clipboard, and marks it transient before it
    /// arrives, so clipboard managers that follow nspasteboard.org never see it unmarked.
    fn write(&mut self, pasteboard: Retained<NSPasteboard>, text: &str) -> anyhow::Result<()> {
        pasteboard.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
        // SAFETY: AppKit's exported pasteboard type is an immutable constant.
        let string = unsafe { NSPasteboardTypeString };
        ensure!(
            pasteboard.setString_forType(
                &NSString::new(),
                &NSString::from_str("org.nspasteboard.TransientType")
            ) && pasteboard.setString_forType(&NSString::from_str(text), string),
            "Cannot place the text on the clipboard"
        );
        Ok(())
    }

    fn mark(&mut self) -> isize {
        clipboard_sequence()
    }

    fn holds(&mut self, text: &str, mark: &isize) -> anyhow::Result<bool> {
        Ok(clipboard_sequence() == *mark
            && insertion::holds_text(arboard::Clipboard::new()?.get_text(), text)?
            && clipboard_sequence() == *mark)
    }

    fn paste_keys(&mut self) -> anyhow::Result<Vec<CGEvent>> {
        let source = private_event_source()?;
        let mut events = Vec::with_capacity(2);
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(source.clone(), KeyCode::ANSI_V, down)
                .map_err(|()| anyhow!("Cannot create paste event"))?;
            event.set_flags(CGEventFlags::CGEventFlagCommand);
            event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
            events.push(event);
        }
        Ok(events)
    }

    fn text_keys(&mut self, text: &str) -> anyhow::Result<Vec<CGEvent>> {
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
        Ok(events)
    }

    fn external_focus(&mut self) -> bool {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .is_some_and(|app| i64::from(app.processIdentifier()) != i64::from(std::process::id()))
    }

    fn shortcut_released(&mut self) -> bool {
        !modifiers().any()
    }

    fn send(&mut self, keys: Vec<CGEvent>) -> anyhow::Result<Inserted> {
        for event in keys {
            event.post(CGEventTapLocation::HID);
        }
        Ok(Inserted::Sent)
    }
}

fn private_event_source() -> anyhow::Result<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|()| anyhow!("Cannot create keyboard event source"))
}

fn clipboard_sequence() -> isize {
    NSPasteboard::generalPasteboard().changeCount()
}

pub(super) fn reduced_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

pub(super) fn show_startup_error(message: &str) {
    if let Some(main) = MainThreadMarker::new() {
        let alert = NSAlert::new(main);
        alert.setMessageText(&NSString::from_str("Speakeasy could not start"));
        alert.setInformativeText(&NSString::from_str(message));
        alert.runModal();
    }
}
