mod clipboard;
pub(crate) mod movie;
pub(crate) mod screen;
mod status_item;

use std::{
    cell::OnceCell,
    ffi::c_void,
    fs::{self, File},
    io,
    os::{fd::AsFd as _, unix::fs::MetadataExt as _},
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
use dispatch2::DispatchQueue;
use objc2::{MainThreadMarker, MainThreadOnly as _, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSAlert, NSEvent, NSFloatingWindowLevel, NSPasteboard, NSScreen, NSView, NSWindow,
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

const OWN_INPUT: i64 = 0x5350_4541;

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
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    let shortcut = options.shortcut_key.mac;
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
        policy: Mutex::new(keyboard::Mac::new(shortcut)),
        typed,
    });
    let control = Arc::new(MonitorControl::default());
    let observers = lifecycle_observers(&input, &policy, &control);
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
        if !started || self.input.is_closed() {
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
    typed: insertion::KeyCount,
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
    let screen_locked = ns_string!("com.apple.screenIsLocked");
    [
        Observer::interrupt_on(workspace.clone(), will_sleep, input, policy, control),
        Observer::interrupt_on(workspace, resigned, input, policy, control),
        Observer::interrupt_on(distributed, screen_locked, input, policy, control),
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
    run_while_open(input);
    port.set_enabled(false);
    run_loop.remove_observer(&observer, modes);
    run_loop.remove_source(&source, modes);
    Ok(())
}

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
    let mut policy = policy.lock();
    if matches!(
        kind,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        policy.missed_events(input);
        typed.add();
        if !port.set_enabled(true) {
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
    let decision = policy.observe(key_event, || shortcut_key_down(shortcut), Instant::now());
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
        .find(|screen| NSMouseInRect(pointer, screen.frame(), false))?;
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

fn modifiers_for(shortcut: MacShortcut) -> Modifiers {
    let mut held = shortcut.modifiers(
        cg::CGEventSource::flags_state(cg::CGEventSourceStateID::CombinedSessionState).0,
    );
    if shortcut == MacShortcut::F13 {
        held.talk = shortcut_key_down(shortcut);
    }
    held
}

fn shortcut_key_down(shortcut: MacShortcut) -> bool {
    u16::try_from(shortcut.key_code()).is_ok_and(|code| {
        cg::CGEventSource::key_state(cg::CGEventSourceStateID::HIDSystemState, code)
    })
}

struct Insert {
    shortcut: MacShortcut,
    typed: insertion::KeyCount,
}

impl insertion::NativeInsert for Insert {
    type Activity = (u64, [f64; 2], [u32; 3]);
    type Contents = clipboard::Saved;
    type Keys = Vec<CGEvent>;
    type Lease = clipboard::Lease;
    type Mark = isize;
    type Target = i32;

    fn acquire(&mut self) -> anyhow::Result<clipboard::Lease> {
        Ok(clipboard::acquire())
    }

    fn save(&mut self, lease: &clipboard::Lease) -> Option<clipboard::Saved> {
        clipboard::save(lease)
    }

    fn write(
        &mut self,
        lease: clipboard::Lease,
        text: &str,
        saved: Option<&clipboard::Saved>,
    ) -> anyhow::Result<Self::Mark> {
        clipboard::write(lease, text, saved)
    }

    fn restore(&mut self, contents: Option<clipboard::Saved>, mark: &isize) -> anyhow::Result<()> {
        objc2::rc::autoreleasepool(|_| {
            clipboard::restore(clipboard::acquire(), contents.as_ref(), *mark)
        })
    }

    fn mark(&mut self) -> isize {
        change_count()
    }

    fn holds(&mut self, text: &str, mark: &isize) -> anyhow::Result<bool> {
        Ok(clipboard::holds(&clipboard::acquire(), text, *mark))
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
        for typed in insertion::split_line_breaks(text) {
            match typed {
                insertion::Typed::Text(run) => {
                    for chunk in insertion::utf16_chunks(run, TEXT_EVENT_UNITS) {
                        let press =
                            own_key_event(&source, 0, true).context("Cannot create text event")?;
                        press.set_string(chunk);
                        let release = own_key_event(&source, 0, false)
                            .context("Cannot create text release")?;
                        events.extend([press, release]);
                    }
                },
                insertion::Typed::LineBreak => {
                    for down in [true, false] {
                        let event = own_key_event(&source, KeyCode::RETURN, down)
                            .context("Cannot create line break event")?;
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
                own_key_event(&source, KeyCode::RETURN, down).context("Cannot create Return event")
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

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

pub(super) fn secure_input() -> bool {
    MainThreadMarker::new().is_some_and(|_| unsafe { IsSecureEventInputEnabled() } != 0)
}

const SECURE_INPUT_PATIENCE: Duration = Duration::from_millis(50);

fn secure_input_from_worker() -> Option<bool> {
    let (reply, answer) = mpsc::sync_channel(1);
    DispatchQueue::main().exec_async(move || {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A worker that stopped waiting dropped its receiver and already treats the answer as unknown"
        )]
        let _ = reply.try_send(secure_input());
    });
    answer.recv_timeout(SECURE_INPUT_PATIENCE).ok()
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
        alert.setMessageText(ns_string!("Speakeasy could not start"));
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
