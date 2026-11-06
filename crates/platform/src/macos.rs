mod clipboard;
pub(crate) mod movie;
pub(crate) mod screen;

use std::{
    cell::OnceCell,
    ffi::c_void,
    fs::{self, File},
    io,
    os::{fd::AsFd as _, unix::fs::MetadataExt as _},
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
        CGEventTapPlacement, CGEventType, CGKeyCode, CallbackResult, EventField, KeyCode,
    },
    event_source::{CGEventSource, CGEventSourceStateID},
};
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSAlert, NSEvent, NSFloatingWindowLevel, NSPasteboard, NSScreen, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
    NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceWillSleepNotification,
};
use objc2_core_graphics as cg;
use objc2_foundation::{
    NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSObjectProtocol,
    NSPoint, NSString,
};
use raw_window_handle::{RawWindowHandle, WindowHandle};

pub(super) use self::clipboard::{copy_content, copy_text};
use super::{
    CANCEL_SHORTCUT, DesktopOptions, Input, InputSender, Inserted, Inserter, MacShortcut,
    NativeWindow, OwnFocus, OwnedThread, PILL_MARGIN, PILL_WIDTH, insertion,
    keyboard::{self, MacEvent, Modifiers},
    monitor::MonitorControl,
    screen::geometry::Bounds,
};

const OWN_INPUT: i64 = 0x5350_4541;

const TEXT_EVENT_UNITS: usize = 16;

pub(super) struct Monitor {
    control: Arc<MonitorControl<CFRunLoop>>,
    _observers: Vec<Observer>,
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
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    let shortcut = options.mac_shortcut;
    let (insertion, inserter) =
        insertion::Worker::start(input.clone(), own_focus, move |request| {
            objc2::rc::autoreleasepool(|_| insertion::deliver(&mut Insert { shortcut }, request))
        })?;
    #[expect(
        clippy::disallowed_types,
        reason = "The event tap and AppKit lifecycle callbacks share the same narrowly locked policy; see SharedPolicy"
    )]
    let policy = Arc::new(SharedPolicy {
        shortcut: options.mac_shortcut,
        policy: Mutex::new(keyboard::Mac::new(options.mac_shortcut)),
    });
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

struct RunLoopReady<'a> {
    shortcut: MacShortcut,
    control: &'a MonitorControl<CFRunLoop>,
    input: &'a InputSender,
}

impl RunLoopReady<'_> {
    fn publish(&self) {
        let run_loop = CFRunLoop::get_current();
        let started = self.control.start(run_loop.clone(), || {
            self.input.deliver(Input::DesktopReady {
                shortcut: self.shortcut.label().into(),
                cancel: CANCEL_SHORTCUT.into(),
            });
        });
        if !started {
            run_loop.stop();
        }
    }
}

struct SharedPolicy {
    shortcut: MacShortcut,
    #[expect(
        clippy::disallowed_types,
        reason = "AppKit posts lifecycle notifications on its own thread while the event tap decides keys on its run loop, which cannot receive them as events; deciding and delivering under one lock keeps the owner's inputs in decision order"
    )]
    policy: Mutex<keyboard::Mac>,
}

impl SharedPolicy {
    fn lock(&self) -> MutexGuard<'_, keyboard::Mac> {
        self.policy.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

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
            let interrupted =
                panic::catch_unwind(AssertUnwindSafe(|| policy.lock().interrupt(&input)));
            if interrupted.is_err() {
                input.close();
            }
        });

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

fn lifecycle_observers(input: &InputSender, policy: &Arc<SharedPolicy>) -> Vec<Observer> {
    let workspace = NSWorkspace::sharedWorkspace().notificationCenter();

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

const UNWATCHED: &str = "Cannot watch the dictation shortcut. Resume dictation to try again.";

fn monitor(input: &InputSender, control: &MonitorControl<CFRunLoop>, policy: &SharedPolicy) {
    let watched = panic::catch_unwind(AssertUnwindSafe(|| run_event_loop(input, control, policy)))
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
    let source = tap
        .mach_port()
        .create_runloop_source(0)
        .map_err(|()| anyhow!(UNWATCHED))?;
    let run_loop = CFRunLoop::get_current();
    let mut ready = RunLoopReady {
        control,
        input,
        shortcut: policy.shortcut,
    };
    let mut context = CFRunLoopObserverContext {
        version: 0,
        info: (&raw mut ready).cast(),
        retain: None,
        release: None,
        copyDescription: None,
    };

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

    let observer = unsafe { CFRunLoopObserver::wrap_under_create_rule(raw) };

    let modes = unsafe { kCFRunLoopCommonModes };
    run_loop.add_source(&source, modes);
    run_loop.add_observer(&observer, modes);
    tap.enable();
    CFRunLoop::run_current();
    run_loop.remove_observer(&observer, modes);
    run_loop.remove_source(&source, modes);
    Ok(())
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

    fn reenable(&self) -> bool {
        let Some(port) = self.port.get() else {
            return false;
        };

        let port = unsafe {
            &*port
                .as_concrete_TypeRef()
                .cast::<objc2_core_foundation::CFMachPort>()
        };
        cg::CGEvent::tap_enable(port, true);
        cg::CGEvent::tap_is_enabled(port)
    }
}

fn shortcut_tap<'tap>(
    input: &'tap InputSender,
    policy: &'tap SharedPolicy,
    port: &'tap TapPort,
) -> anyhow::Result<CGEventTap<'tap>> {
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
                panic::catch_unwind(AssertUnwindSafe(|| {
                    observe(input, &mut policy.lock(), policy.shortcut, port, kind, event)
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

extern "C" fn on_run_loop_entry(
    _: CFRunLoopObserverRef,
    _: CFRunLoopActivity,
    context: *mut c_void,
) {
    let ready = unsafe { &*context.cast::<RunLoopReady<'_>>() };
    if panic::catch_unwind(AssertUnwindSafe(|| ready.publish())).is_err() {
        ready.input.close();
        CFRunLoop::get_current().stop();
    }
}

fn observe(
    input: &InputSender,
    policy: &mut keyboard::Mac,
    shortcut: MacShortcut,
    port: &TapPort,
    kind: CGEventType,
    event: &CGEvent,
) -> CallbackResult {
    if matches!(
        kind,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        policy.missed_events(input);
        if !port.reenable() {
            input.close();
        }
        return CallbackResult::Keep;
    }
    if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == OWN_INPUT {
        return CallbackResult::Keep;
    }
    let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);

    let held = shortcut.modifiers(event.get_flags().bits());
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
    let decision = policy.observe(
        key_event,
        || {
            u16::try_from(shortcut.key_code()).is_ok_and(|code| {
                cg::CGEventSource::key_state(cg::CGEventSourceStateID::HIDSystemState, code)
            })
        },
        Instant::now(),
    );
    decision.deliver(input);
    if decision.swallows() {
        CallbackResult::Drop
    } else {
        CallbackResult::Keep
    }
}

impl NativeWindow {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "Windows and X11 configuration can fail; callers share one signature on every platform"
    )]
    pub fn configure_pill(&self) -> anyhow::Result<()> {
        let window = &self.native.ns_window;
        window.setIgnoresMouseEvents(true);

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
            if let Some(origin) = pill_origin_under_pointer() {
                window.setFrameOrigin(origin);
            }
            window.orderFrontRegardless();
        } else {
            window.orderOut(None);
        }
    }
}

fn pill_origin_under_pointer() -> Option<NSPoint> {
    let main = MainThreadMarker::new()?;
    let pointer = NSEvent::mouseLocation();
    let screen = NSScreen::screens(main)
        .iter()
        .find(|screen| Bounds::from(screen.frame()).contains([pointer.x, pointer.y]))?;
    let frame = screen.visibleFrame();
    Some(NSPoint::new(
        frame.origin.x + (frame.size.width - f64::from(PILL_WIDTH)) / 2.0,
        frame.origin.y + f64::from(PILL_MARGIN),
    ))
}

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

        let view = unsafe { view.ns_view.cast::<NSView>().as_ref() };
        let ns_window = view.window().context("The view has no window")?;
        Ok(Self { ns_window })
    }
}

pub(super) fn modifiers_for(shortcut: MacShortcut) -> Modifiers {
    let mut held = shortcut.modifiers(
        cg::CGEventSource::flags_state(cg::CGEventSourceStateID::CombinedSessionState).0,
    );
    if shortcut == MacShortcut::F13 {
        held.talk = u16::try_from(shortcut.key_code()).is_ok_and(|code| {
            cg::CGEventSource::key_state(cg::CGEventSourceStateID::HIDSystemState, code)
        });
    }
    held
}

pub(super) struct Insert {
    shortcut: MacShortcut,
}

impl insertion::NativeInsert for Insert {
    type Keys = Vec<CGEvent>;
    type Lease = clipboard::Lease;
    type Mark = isize;

    fn acquire(&mut self) -> anyhow::Result<clipboard::Lease> {
        clipboard::acquire()
    }

    fn write(&mut self, lease: clipboard::Lease, text: &str) -> anyhow::Result<()> {
        clipboard::write(lease, text)
    }

    fn mark(&mut self) -> isize {
        change_count()
    }

    fn holds(&mut self, text: &str, mark: &isize) -> anyhow::Result<bool> {
        Ok(change_count() == *mark
            && insertion::holds_text(arboard::Clipboard::new()?.get_text(), text)?
            && change_count() == *mark)
    }

    fn paste_keys(&mut self) -> anyhow::Result<Vec<CGEvent>> {
        let source = private_event_source()?;
        [true, false]
            .into_iter()
            .map(|down| {
                let event = own_key_event(&source, KeyCode::ANSI_V, down)
                    .context("Cannot create paste event")?;
                event.set_flags(CGEventFlags::CGEventFlagCommand);
                Ok(event)
            })
            .collect()
    }

    fn text_keys(&mut self, text: &str) -> anyhow::Result<Vec<CGEvent>> {
        let source = private_event_source()?;
        let mut events = Vec::new();
        for chunk in insertion::utf16_chunks(text, TEXT_EVENT_UNITS) {
            let press = own_key_event(&source, 0, true).context("Cannot create text event")?;
            press.set_string(chunk);
            let release = own_key_event(&source, 0, false).context("Cannot create text release")?;
            events.extend([press, release]);
        }
        Ok(events)
    }

    fn external_focus(&mut self) -> bool {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .is_some_and(|app| !is_own_process(app.processIdentifier().into()))
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
        .map_err(|()| anyhow!("Cannot create keyboard event source"))
}

fn own_key_event(source: &CGEventSource, code: CGKeyCode, down: bool) -> Option<CGEvent> {
    let event = CGEvent::new_keyboard_event(source.clone(), code, down).ok()?;
    event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
    Some(event)
}

fn is_own_process(pid: i64) -> bool {
    pid == i64::from(std::process::id())
}

fn change_count() -> isize {
    NSPasteboard::generalPasteboard().changeCount()
}

pub(super) fn reduced_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

pub(super) fn attach_stdio() -> bool {
    const LAUNCHD: u32 = 1;
    std::os::unix::process::parent_id() != LAUNCHD || !discards_stderr()
}

fn discards_stderr() -> bool {
    let stderr = io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .and_then(|fd| File::from(fd).metadata());
    let (Ok(stderr), Ok(null)) = (stderr, fs::metadata("/dev/null")) else {
        return true;
    };
    (stderr.dev(), stderr.ino()) == (null.dev(), null.ino())
}

pub(super) fn show_startup_error(message: &str) {
    if let Some(main) = MainThreadMarker::new() {
        let alert = NSAlert::new(main);
        alert.setMessageText(&NSString::from_str("Speakeasy could not start"));
        alert.setInformativeText(&NSString::from_str(message));
        alert.runModal();
    }
}
