//! macOS adapter: the shortcut event tap, lifecycle interrupts, windows, and insertion.
//!
//! The tap, run-loop observer, and notification callbacks catch panics. A panic closes the input
//! lane, and a closed lane stops the tap's run loop.

mod clipboard;
pub(crate) mod movie;
pub(crate) mod screen;
mod status_item;

use std::{
    cell::OnceCell,
    ffi::c_void,
    panic::{self, AssertUnwindSafe},
    pin::pin,
    ptr::NonNull,
    sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc},
    task::{Context, Wake, Waker},
    time::{Duration, Instant},
};

use anyhow::{Context as _, anyhow, bail, ensure};
use block2::RcBlock;
use core_foundation::{
    array::CFArray,
    base::{CFType, TCFType, kCFAllocatorDefault},
    boolean::CFBoolean,
    data::CFData,
    dictionary::CFDictionary,
    mach_port::CFMachPort,
    number::CFNumber,
    runloop::{
        CFRunLoop, CFRunLoopActivity, CFRunLoopObserver, CFRunLoopObserverContext,
        CFRunLoopObserverCreate, CFRunLoopObserverRef, kCFRunLoopCommonModes, kCFRunLoopEntry,
    },
    string::{CFString, CFStringRef},
};
use core_graphics::{
    event::{
        CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventType, CGKeyCode, CallbackResult, EventField, KeyCode,
    },
    event_source::{CGEventSource, CGEventSourceStateID},
};
use dispatch2::DispatchQueue;
use objc2::{MainThreadMarker, MainThreadOnly as _, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSAlert, NSEvent, NSFloatingWindowLevel, NSRunningApplication, NSScreen, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
    NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceWillSleepNotification,
};
use objc2_core_graphics as cg;
use objc2_foundation::{
    NSDistributedNotificationCenter, NSMouseInRect, NSNotification, NSNotificationCenter,
    NSObjectProtocol, NSPoint, NSString, ns_string,
};
use raw_window_handle::{RawWindowHandle, WindowHandle};

pub(super) use self::clipboard::{copy_content, copy_text};
pub use self::status_item::StatusItem;
use super::{
    CANCEL_SHORTCUT, DesktopOptions, Input, InputSender, Inserted, Inserter, MacShortcut,
    NativeWindow, OwnFocus, OwnedThread, PILL_MARGIN, PILL_WIDTH, insertion,
    keyboard::{self, MacEvent, Modifiers},
    monitor::MonitorControl,
};

/// Tags events Speakeasy posts ("SPEA" in ASCII) so its own tap passes over them.
const OWN_INPUT: i64 = 0x5350_4541;

/// UTF-16 units per text event; Core Graphics truncates longer keyboard-event strings.
const TEXT_EVENT_UNITS: usize = 16;

pub(super) struct Monitor {
    control: Arc<MonitorControl<CFRunLoop>>,
    _observers: [Observer; 3],
    insertion: insertion::Worker,
}

impl Monitor {
    pub(super) fn request_stop(&self) {
        self.control.request_stop(CFRunLoop::stop);
        self.insertion.request_stop();
    }

    pub(super) fn stopped(&self) -> impl Future<Output = ()> + Send + use<> {
        self.insertion.stopped()
    }
}

pub(super) fn start(
    input: InputSender,
    options: DesktopOptions,
    own_focus: OwnFocus,
    recording: Arc<keyboard::Recording>,
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    let shortcuts = options.mac;
    let shortcut = shortcuts.talk;
    // Text Input Sources answer only on the main thread, so the layout-aware label is read here.
    let label = shortcut.label();
    let typed = insertion::KeyCount::default();
    let (insertion, inserter) = insertion::Worker::start(input.clone(), own_focus, {
        let typed = typed.clone();
        let mut continuation = insertion::Continuation::default();
        move |request| {
            let native = Insert {
                shortcut,
                typed: typed.clone(),
            };
            objc2::rc::autoreleasepool(|_| insertion::settle(native, request, &mut continuation))
        }
    })?;
    #[expect(
        clippy::disallowed_types,
        reason = "The event tap and AppKit lifecycle callbacks share the same narrowly locked policy; see SharedPolicy"
    )]
    let policy = Arc::new(SharedPolicy {
        shortcut,
        policy: Mutex::new(keyboard::Mac::new(shortcuts)),
        typed,
        recording,
    });
    let control = Arc::new(MonitorControl::default());
    let observers = lifecycle_observers(&input, &policy, &control);
    let thread = OwnedThread::spawn("input-monitor", {
        let control = Arc::clone(&control);
        move || monitor(&input, &control, &policy, &label)
    })?;
    let monitor = Monitor {
        control,
        _observers: observers,
        insertion,
    };
    Ok((thread, monitor, inserter))
}

struct RunLoopReady<'a> {
    shortcut: MacShortcut,
    label: &'a str,
    control: &'a MonitorControl<CFRunLoop>,
    input: &'a InputSender,
}

impl RunLoopReady<'_> {
    fn publish(&self) {
        let run_loop = CFRunLoop::get_current();
        let started = self.control.start(run_loop.clone(), || {
            self.input.deliver(Input::DesktopReady {
                shortcut: self.label.to_owned(),
                cancel: CANCEL_SHORTCUT.into(),
                globe_shortcut: self.shortcut == MacShortcut::FUNCTION,
            });
        });
        if !started || self.input.is_closed() {
            run_loop.stop();
        }
    }
}

struct SharedPolicy {
    /// The talk shortcut.
    shortcut: MacShortcut,
    #[expect(
        clippy::disallowed_types,
        reason = "AppKit posts lifecycle notifications on its own thread while the event tap decides keys on its run loop, which cannot receive them as events; deciding and delivering under one lock keeps the owner's inputs in decision order"
    )]
    policy: Mutex<keyboard::Mac>,
    typed: insertion::KeyCount,
    recording: Arc<keyboard::Recording>,
}

impl SharedPolicy {
    /// Ignores poison: the panic that poisoned the lock already closed the input lane.
    fn lock(&self) -> MutexGuard<'_, keyboard::Mac> {
        self.policy.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A block observer, registered until `Drop` removes its token.
///
/// The center holds the token and block until removal, so dropping the token alone would leave
/// the block running.
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
        control: &Arc<MonitorControl<CFRunLoop>>,
    ) -> Self {
        let input = input.clone();
        let policy = Arc::clone(policy);
        let control = Arc::clone(control);
        let callback = RcBlock::new(move |_: NonNull<NSNotification>| {
            let interrupted =
                panic::catch_unwind(AssertUnwindSafe(|| policy.lock().interrupt(&input)));
            if interrupted.is_err() {
                input.close();
            }
            if input.is_closed() {
                control.request_stop(CFRunLoop::stop);
            }
        });

        // SAFETY: the block is sendable: it touches only the locked policy, the input lane, and
        // the stop request, never an AppKit object. With no queue it runs on the posting thread.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &callback)
        };
        Self { center, token }
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        unsafe {
            self.center.removeObserver((*self.token).as_ref());
        }
    }
}

fn lifecycle_observers(
    input: &InputSender,
    policy: &Arc<SharedPolicy>,
    control: &Arc<MonitorControl<CFRunLoop>>,
) -> [Observer; 3] {
    let workspace = NSWorkspace::sharedWorkspace().notificationCenter();

    let (will_sleep, resigned) = unsafe {
        (
            NSWorkspaceWillSleepNotification,
            NSWorkspaceSessionDidResignActiveNotification,
        )
    };
    let distributed = NSDistributedNotificationCenter::defaultCenter().into_super();
    // Screen lock has no public notification name; the system posts this distributed one.
    let screen_locked = ns_string!("com.apple.screenIsLocked");
    [
        Observer::interrupt_on(workspace.clone(), will_sleep, input, policy, control),
        Observer::interrupt_on(workspace, resigned, input, policy, control),
        Observer::interrupt_on(distributed, screen_locked, input, policy, control),
    ]
}

const UNWATCHED: &str = "Can't watch the dictation shortcut. Resume dictation to try again.";

fn monitor(
    input: &InputSender,
    control: &MonitorControl<CFRunLoop>,
    policy: &SharedPolicy,
    label: &str,
) {
    let watched = panic::catch_unwind(AssertUnwindSafe(|| {
        run_event_loop(input, control, policy, label)
    }))
    .unwrap_or_else(|_| Err(anyhow!(UNWATCHED)));
    if let Err(error) = watched
        && !control.stopping()
        && !input.is_closed()
    {
        input.deliver(Input::Unavailable(error.to_string()));
    }
    control.retire();
    input.close();
}

/// Runs the shortcut tap on this thread's run loop until it stops.
///
/// The tap, its source, and the entry observer live in this frame through the run, and the
/// observer is removed before the stack `ready` it borrows goes away.
fn run_event_loop(
    input: &InputSender,
    control: &MonitorControl<CFRunLoop>,
    policy: &SharedPolicy,
    label: &str,
) -> anyhow::Result<()> {
    if control.stopping() {
        return Ok(());
    }
    let port = TapPort::default();
    let tap = shortcut_tap(input, policy, &port)?;
    port.publish(tap.mach_port());
    let source = tap
        .mach_port()
        .create_runloop_source(0)
        .map_err(|()| anyhow!(UNWATCHED))?;
    let run_loop = CFRunLoop::get_current();
    let mut ready = RunLoopReady {
        control,
        input,
        shortcut: policy.shortcut,
        label,
    };
    let mut context = CFRunLoopObserverContext {
        version: 0,
        info: (&raw mut ready).cast(),
        retain: None,
        release: None,
        copyDescription: None,
    };

    // SAFETY: `ready` outlives the run, and the observer is removed before this frame ends.
    let raw = unsafe {
        CFRunLoopObserverCreate(
            kCFAllocatorDefault,
            kCFRunLoopEntry,
            0,
            0,
            on_run_loop_entry,
            &raw mut context,
        )
    };
    ensure!(!raw.is_null(), UNWATCHED);

    // SAFETY: `raw` is a non-null Create-rule observer, adopted exactly once here.
    let observer = unsafe { CFRunLoopObserver::wrap_under_create_rule(raw) };

    let modes = unsafe { kCFRunLoopCommonModes };
    run_loop.add_source(&source, modes);
    run_loop.add_observer(&observer, modes);
    tap.enable();
    run_while_open(input);
    port.set_enabled(false);
    run_loop.remove_observer(&observer, modes);
    run_loop.remove_source(&source, modes);
    Ok(())
}

/// Stops the run loop when the input lane closes.
///
/// `wake` takes no lock: the lane can close while the policy or `MonitorControl` lock is held.
/// A stop before the loop enters is lost; `on_run_loop_entry` rechecks the lane on entry.
struct StopOnClose(CFRunLoop);

impl Wake for StopOnClose {
    fn wake(self: Arc<Self>) {
        self.0.stop();
    }
}

fn run_while_open(input: &InputSender) {
    let waker = Waker::from(Arc::new(StopOnClose(CFRunLoop::get_current())));
    let mut closed = pin!(input.closed());
    if closed
        .as_mut()
        .poll(&mut Context::from_waker(&waker))
        .is_pending()
    {
        CFRunLoop::run_current();
    }
}

#[derive(Default)]
struct TapPort {
    #[expect(
        clippy::disallowed_types,
        reason = "macOS returns the tap's port only once the callback that needs it is registered"
    )]
    port: OnceCell<CFMachPort>,
}

impl TapPort {
    fn publish(&self, port: &CFMachPort) {
        self.port.get_or_init(|| port.clone());
    }

    fn set_enabled(&self, enabled: bool) -> bool {
        let Some(port) = self.port.get() else {
            return false;
        };

        // SAFETY: both bindings name the same `CFMachPort`, which `self` keeps alive for this
        // borrow.
        let port = unsafe {
            &*port
                .as_concrete_TypeRef()
                .cast::<objc2_core_foundation::CFMachPort>()
        };
        cg::CGEvent::tap_enable(port, enabled);
        cg::CGEvent::tap_is_enabled(port)
    }
}

fn shortcut_tap<'tap>(
    input: &'tap InputSender,
    policy: &'tap SharedPolicy,
    port: &'tap TapPort,
) -> anyhow::Result<CGEventTap<'tap>> {
    let callback = move |_, kind, event: &CGEvent| {
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            if input.is_closed() {
                CallbackResult::Keep
            } else {
                observe(input, policy, port, kind, event)
            }
        }))
        .unwrap_or_else(|_| {
            input.close();
            CallbackResult::Keep
        });
        if input.is_closed() {
            port.set_enabled(false);
            CFRunLoop::get_current().stop();
            CallbackResult::Keep
        } else {
            result
        }
    };
    let events = vec![
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
    ];
    // SAFETY: `run_event_loop` drops the tap before the state it borrows and adds its source
    // only to this thread's run loop, so the callback runs here while that state lives.
    unsafe {
        CGEventTap::new_unchecked(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            events,
            callback,
        )
    }
    .map_err(|()| {
        anyhow!(
            "Allow Speakeasy in System Settings › Privacy & Security › Accessibility. If it is already on, remove it with − and add it again, then resume dictation."
        )
    })
}

/// Publishes the run loop to `MonitorControl` once it has entered.
///
/// `CFRunLoopStop` before entry does not stop a later run, so a stop requested earlier, or a
/// lane closed earlier, ends the run from here.
extern "C" fn on_run_loop_entry(
    _: CFRunLoopObserverRef,
    _: CFRunLoopActivity,
    context: *mut c_void,
) {
    // SAFETY: `context` is `run_event_loop`'s `ready`, which outlives this observer.
    let ready = unsafe { &*context.cast::<RunLoopReady<'_>>() };
    if panic::catch_unwind(AssertUnwindSafe(|| ready.publish())).is_err() {
        ready.input.close();
        CFRunLoop::get_current().stop();
    }
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "Delivery stays under the policy lock so the owner receives inputs in decision order"
)]
fn observe(
    input: &InputSender,
    policy: &SharedPolicy,
    port: &TapPort,
    kind: CGEventType,
    event: &CGEvent,
) -> CallbackResult {
    let shortcut = policy.shortcut;
    let typed = &policy.typed;
    let recording = &policy.recording;
    let mut policy = policy.lock();
    if matches!(
        kind,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        if !port.set_enabled(true) {
            input.close();
        }
        return CallbackResult::Keep;
    }
    if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == OWN_INPUT {
        return CallbackResult::Keep;
    }
    let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);

    // Both Core Graphics bindings use the same `CGEventFlags` bits; see `modifiers_for`.
    let flags = event.get_flags().bits();
    let held = shortcut.modifiers(flags);
    let key_event = match kind {
        CGEventType::KeyDown => MacEvent::Key {
            code,
            down: true,
            repeat: event.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT) != 0,
            held,
        },
        CGEventType::KeyUp => MacEvent::Key {
            code,
            down: false,
            repeat: false,
            held,
        },
        CGEventType::FlagsChanged => MacEvent::FlagsChanged { code, held },
        _ => return CallbackResult::Keep,
    };
    if let Some(swallow) = recording.observe(keyboard::Mac::recorded(key_event, flags)) {
        return if swallow {
            CallbackResult::Drop
        } else {
            CallbackResult::Keep
        };
    }
    let decision = policy.observe(key_event, || shortcut_down(shortcut), Instant::now());
    decision.deliver(input);
    if keyboard::Mac::types(key_event, &decision) {
        typed.add();
    }
    if decision.swallows() {
        CallbackResult::Drop
    } else {
        CallbackResult::Keep
    }
}

impl NativeWindow {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "Windows pill configuration can fail; the app configures the pill through one signature on Windows and macOS"
    )]
    pub fn configure_pill(&self) -> anyhow::Result<()> {
        let window = &self.native.ns_window;
        window.setIgnoresMouseEvents(true);

        // A titled panel highlights its top edge in dark mode and shadows the whole transparent
        // window; the capsule draws its own shadow.
        window.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
        window.setHasShadow(false);
        window.setLevel(NSFloatingWindowLevel);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        Ok(())
    }

    pub fn set_window_visible(&self, visible: bool) {
        let window = &self.native.ns_window;
        if visible {
            window.deminiaturize(None);
            window.makeKeyAndOrderFront(None);
        } else {
            window.orderOut(None);
        }
    }

    pub fn set_pill_visible(&self, visible: bool) {
        let window = &self.native.ns_window;
        if visible {
            if let Some(origin) = pill_origin_under_pointer(window.mtm()) {
                window.setFrameOrigin(origin);
            }
            // Raises the pill while Speakeasy is inactive, without changing the key window.
            window.orderFrontRegardless();
        } else {
            window.orderOut(None);
        }
    }
}

fn pill_origin_under_pointer(main: MainThreadMarker) -> Option<NSPoint> {
    let pointer = NSEvent::mouseLocation();
    let screen = NSScreen::screens(main)
        .iter()
        // `mouseLocation` puts the top row at the frame's max y, which a half-open test misses;
        // `NSMouseInRect` applies AppKit's edge rule.
        .find(|screen| NSMouseInRect(pointer, screen.frame(), false))?;
    let frame = screen.visibleFrame();
    Some(NSPoint::new(
        frame.origin.x + (frame.size.width - f64::from(PILL_WIDTH)) / 2.0,
        frame.origin.y + f64::from(PILL_MARGIN),
    ))
}

/// A GPUI window's retained `NSWindow`; main-thread-only, so this is not `Send`.
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

        // SAFETY: the borrowed handle keeps its `NSView` alive, and this is the main thread.
        let view = unsafe { view.ns_view.cast::<NSView>().as_ref() };
        let ns_window = view.window().context("The view has no window")?;
        Ok(Self { ns_window })
    }
}

fn modifiers_for(shortcut: MacShortcut) -> Modifiers {
    let mut held = shortcut.modifiers(
        cg::CGEventSource::flags_state(cg::CGEventSourceStateID::CombinedSessionState).0,
    );
    // A shortcut's key sets no modifier flag, so its hold comes from the HID key state.
    held.talk = held.talk || shortcut.chord().code().is_some_and(key_down);
    held
}

/// Whether every key of the talk shortcut is physically down.
fn shortcut_down(shortcut: MacShortcut) -> bool {
    keyboard::Mac::chord_down(shortcut.chord(), key_down)
}

fn key_down(code: u16) -> bool {
    cg::CGEventSource::key_state(cg::CGEventSourceStateID::HIDSystemState, code)
}

struct Insert {
    shortcut: MacShortcut,
    typed: insertion::KeyCount,
}

impl insertion::NativeInsert for Insert {
    type Activity = (u64, [f64; 2], [u32; 3]);
    type Contents = clipboard::Saved;
    type Keys = Vec<CGEvent>;
    type Mark = isize;
    type Target = i32;

    fn copy(&mut self, text: &str) -> anyhow::Result<(isize, Option<clipboard::Saved>)> {
        let saved = clipboard::snapshot();
        Ok((clipboard::write(text)?, saved))
    }

    fn restore(&mut self, contents: Option<clipboard::Saved>, mark: &isize) -> anyhow::Result<()> {
        objc2::rc::autoreleasepool(|_| clipboard::restore(contents.as_ref(), *mark))
    }

    fn paste_keys(&mut self) -> anyhow::Result<Vec<CGEvent>> {
        let source = private_event_source()?;
        [true, false]
            .into_iter()
            .map(|down| {
                let event = own_key_event(&source, KeyCode::ANSI_V, down)
                    .context("Can't create paste event")?;
                event.set_flags(CGEventFlags::CGEventFlagCommand);
                Ok(event)
            })
            .collect()
    }

    fn text_keys(&mut self, text: &str) -> anyhow::Result<Vec<CGEvent>> {
        let source = private_event_source()?;
        let mut events = Vec::new();
        for typed in insertion::split_line_breaks(text) {
            match typed {
                insertion::Typed::Text(run) => {
                    for chunk in insertion::utf16_chunks(run, TEXT_EVENT_UNITS) {
                        let press =
                            own_key_event(&source, 0, true).context("Can't create text event")?;
                        press.set_string(chunk);
                        let release = own_key_event(&source, 0, false)
                            .context("Can't create text release")?;
                        events.extend([press, release]);
                    }
                },
                // A line feed in a string event can act as Return and send a chat message.
                insertion::Typed::LineBreak => {
                    for down in [true, false] {
                        let event = own_key_event(&source, KeyCode::RETURN, down)
                            .context("Can't create line break event")?;
                        event.set_flags(CGEventFlags::CGEventFlagShift);
                        events.push(event);
                    }
                },
            }
        }
        Ok(events)
    }

    fn return_keys(&mut self) -> anyhow::Result<Vec<CGEvent>> {
        let source = private_event_source()?;
        [true, false]
            .into_iter()
            .map(|down| {
                own_key_event(&source, KeyCode::RETURN, down).context("Can't create Return event")
            })
            .collect()
    }

    fn external_focus(&mut self) -> bool {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .is_some_and(|app| !is_own_process(app.processIdentifier().into()))
    }

    fn target(&mut self) -> Option<i32> {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|app| app.processIdentifier())
    }

    fn activity(&mut self) -> Option<Self::Activity> {
        // Secure input hides key presses from the tap, so the typed count cannot vouch for an
        // untouched field; an unknown answer counts as on.
        if secure_input_from_worker() != Some(false) {
            return None;
        }
        let clicks = [
            cg::CGEventType::LeftMouseDown,
            cg::CGEventType::RightMouseDown,
            cg::CGEventType::OtherMouseDown,
        ]
        .map(|kind| {
            cg::CGEventSource::counter_for_event_type(
                cg::CGEventSourceStateID::HIDSystemState,
                kind,
            )
        });
        Some((self.typed.read(), screen::pointer_location()?, clicks))
    }

    fn shortcut_released(&mut self) -> bool {
        !modifiers_for(self.shortcut).any()
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
        .map_err(|()| anyhow!("Can't create keyboard event source"))
}

fn own_key_event(source: &CGEventSource, code: CGKeyCode, down: bool) -> Option<CGEvent> {
    let event = CGEvent::new_keyboard_event(source.clone(), code, down).ok()?;
    event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
    Some(event)
}

fn is_own_process(pid: i64) -> bool {
    pid == i64::from(std::process::id())
}

pub(super) fn reduced_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *const c_void;
    fn TISGetInputSourceProperty(source: *const c_void, key: CFStringRef) -> *const c_void;
    #[link_name = "kTISPropertyUnicodeKeyLayoutData"]
    static UNICODE_KEY_LAYOUT_DATA: CFStringRef;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const c_void,
        code: u16,
        action: u16,
        modifiers: u32,
        keyboard: u32,
        options: u32,
        dead_keys: *mut u32,
        capacity: usize,
        length: *mut usize,
        text: *mut u16,
    ) -> i32;
}

/// The character the current keyboard layout prints on the key with `CGKeyCode` `code`.
///
/// Text Input Sources answer only on the main thread; elsewhere this is `None`, and labels keep
/// US positions.
pub(crate) fn printed_key(code: u16) -> Option<char> {
    /// `kUCKeyActionDisplay`: the key cap, not what a press types.
    const DISPLAY: u16 = 3;
    /// `1 << kUCKeyTranslateNoDeadKeysBit`: a dead key reports its own accent.
    const NO_DEAD_KEYS: u32 = 1;
    MainThreadMarker::new()?;
    // SAFETY: on the main thread, as Text Input Sources require.
    let source = NonNull::new(unsafe { TISCopyCurrentKeyboardLayoutInputSource() }.cast_mut())?;
    // SAFETY: a Create-rule reference, adopted once.
    let source = unsafe { CFType::wrap_under_create_rule(source.as_ptr().cast_const()) };
    // SAFETY: a constant Carbon defines for the process's lifetime.
    let property = unsafe { UNICODE_KEY_LAYOUT_DATA };
    // SAFETY: a Get-rule reference that `source` keeps alive while it is read.
    let data = unsafe { TISGetInputSourceProperty(source.as_CFTypeRef(), property) };
    let data = NonNull::new(data.cast_mut())?;
    // SAFETY: a Get-rule `CFData`, retained here for the read below.
    let data = unsafe { CFData::wrap_under_get_rule(data.as_ptr().cast_const().cast()) };
    // SAFETY: reads the hardware keyboard type and nothing else.
    let keyboard = u32::from(unsafe { LMGetKbdType() });
    let mut dead_keys = 0;
    let mut text = [0_u16; 4];
    let mut length = 0;
    // SAFETY: the layout bytes live in `data`; `text` holds `text.len()` units, and `length`
    // reports how many were written.
    let status = unsafe {
        UCKeyTranslate(
            data.bytes().as_ptr().cast(),
            code,
            DISPLAY,
            0,
            keyboard,
            NO_DEAD_KEYS,
            &raw mut dead_keys,
            text.len(),
            &raw mut length,
            text.as_mut_ptr(),
        )
    };
    if status != 0 {
        return None;
    }
    let mut characters = char::decode_utf16(text.get(..length)?.iter().copied());
    let character = characters.next()?.ok()?;
    (characters.next().is_none() && !character.is_control() && !character.is_whitespace())
        .then_some(character)
}

fn secure_input_on() -> bool {
    // SAFETY: Carbon marks this query not thread safe; the marker proves the main thread.
    MainThreadMarker::new().is_some_and(|_| unsafe { IsSecureEventInputEnabled() } != 0)
}

pub(super) fn secure_input() -> Option<crate::SecureInput> {
    secure_input_on().then(|| crate::SecureInput {
        holder: secure_input_holder(),
    })
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFPreferencesCopyAppValue(key: CFStringRef, application: CFStringRef) -> *const c_void;
    fn CFPreferencesAppSynchronize(application: CFStringRef) -> u8;
}

pub(super) fn globe_key() -> Option<crate::GlobeKey> {
    let key = CFString::from_static_string("AppleFnUsageType");
    let domain = CFString::from_static_string("com.apple.HIToolbox");
    // SAFETY: both strings live through the calls, and the value follows the Create rule, so
    // `CFType` takes its only reference.
    let value = unsafe {
        // Rereads the domain, which this process otherwise caches from its first read, so a change
        // made in System Settings since then counts.
        let _ = CFPreferencesAppSynchronize(domain.as_concrete_TypeRef());
        let value =
            CFPreferencesCopyAppValue(key.as_concrete_TypeRef(), domain.as_concrete_TypeRef());
        (!value.is_null()).then(|| CFType::wrap_under_create_rule(value))?
    };
    match value.downcast::<CFNumber>()?.to_i32()? {
        1 => Some(crate::GlobeKey::InputSource),
        2 => Some(crate::GlobeKey::Emoji),
        3 => Some(crate::GlobeKey::Dictation),
        _ => None,
    }
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegistryGetRootEntry(main_port: u32) -> u32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFStringRef,
        allocator: *const c_void,
        options: u32,
    ) -> *const c_void;
    fn IOObjectRelease(object: u32) -> i32;
}

/// The app the console session's I/O Registry record names as holding secure input.
///
/// The window server keeps that record; `ioreg` shows it as `kCGSSessionSecureInputPID`.
fn secure_input_holder() -> Option<String> {
    let key = |name| CFString::from_static_string(name).into_CFType();
    let (on_console, holder) = (
        key("kCGSSessionOnConsoleKey"),
        key("kCGSSessionSecureInputPID"),
    );
    let pid = console_sessions()?.iter().find_map(|session| {
        // SAFETY: `IOConsoleUsers` holds one dictionary per login session, retained by the array.
        let session =
            unsafe { CFType::wrap_under_get_rule(*session) }.downcast_into::<CFDictionary>()?;
        let value = |key: &CFType| {
            let value = session.find(key.as_CFTypeRef())?;
            // SAFETY: the dictionary retains its values for this borrow, and `CFType` retains again.
            Some(unsafe { CFType::wrap_under_get_rule(*value) })
        };
        value(&on_console)?
            .downcast::<CFBoolean>()
            .is_some_and(bool::from)
            .then(|| value(&holder)?.downcast::<CFNumber>()?.to_i32())
            .flatten()
    })?;
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    Some(app.localizedName()?.to_string())
}

fn console_sessions() -> Option<CFArray> {
    let key = CFString::from_static_string("IOConsoleUsers");
    // SAFETY: port 0 names IOKit's default main port. The root entry is released once read, and
    // the property follows the Create rule, so `CFType` takes its only reference.
    unsafe {
        let root = IORegistryGetRootEntry(0);
        if root == 0 {
            return None;
        }
        let property = IORegistryEntryCreateCFProperty(
            root,
            key.as_concrete_TypeRef(),
            kCFAllocatorDefault.cast(),
            0,
        );
        IOObjectRelease(root);
        (!property.is_null())
            .then(|| CFType::wrap_under_create_rule(property))?
            .downcast_into::<CFArray>()
    }
}

const SECURE_INPUT_PATIENCE: Duration = Duration::from_millis(50);

/// Asks the main queue, where the query is allowed; `None` if it does not answer in time.
fn secure_input_from_worker() -> Option<bool> {
    let (reply, answer) = mpsc::sync_channel(1);
    DispatchQueue::main().exec_async(move || {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A worker that stopped waiting dropped its receiver and already treats the answer as unknown"
        )]
        let _ = reply.try_send(secure_input_on());
    });
    answer.recv_timeout(SECURE_INPUT_PATIENCE).ok()
}

pub(super) fn attach_stdio() -> bool {
    const LAUNCHD: u32 = 1;
    std::os::unix::process::parent_id() != LAUNCHD
}

pub(super) fn show_startup_error(message: &str) {
    if let Some(main) = MainThreadMarker::new() {
        let alert = NSAlert::new(main);
        alert.setMessageText(ns_string!("Speakeasy couldn't start"));
        alert.setInformativeText(&NSString::from_str(message));
        alert.runModal();
    }
}

#[cfg(test)]
mod tests {
    use core_foundation::{
        date::CFDate,
        runloop::{CFRunLoopTimer, CFRunLoopTimerContext, CFRunLoopTimerRef},
    };

    use super::*;

    extern "C" fn running(_: CFRunLoopTimerRef, info: *mut c_void) {
        let running = unsafe { &*info.cast::<mpsc::Sender<()>>() };
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A test that stopped listening has already failed"
        )]
        let _ = running.send(());
    }

    #[test]
    fn closing_an_idle_input_lane_from_another_thread_stops_the_monitor_loop() {
        let (sender, events) = async_channel::bounded(1);
        let input = InputSender::new(sender);
        let (entered, inside) = mpsc::channel::<()>();
        let (returned, stopped) = mpsc::channel();
        let monitor = std::thread::spawn(move || {
            let mut context = CFRunLoopTimerContext {
                version: 0,
                info: (&raw const entered).cast_mut().cast(),
                retain: None,
                release: None,
                copyDescription: None,
            };
            let soon = CFDate::now().abs_time() + 0.05;
            let timer = CFRunLoopTimer::new(soon, 3600.0, 0, 0, running, &raw mut context);
            let run_loop = CFRunLoop::get_current();
            let modes = unsafe { kCFRunLoopCommonModes };
            run_loop.add_timer(&timer, modes);
            run_while_open(&input);
            run_loop.remove_timer(&timer, modes);
            returned.send(()).unwrap();
        });
        inside.recv_timeout(crate::PATIENCE).unwrap();
        drop(events);
        assert!(
            stopped.recv_timeout(crate::PATIENCE).is_ok(),
            "The monitor kept running after its input lane closed"
        );
        monitor.join().unwrap();
    }
}
