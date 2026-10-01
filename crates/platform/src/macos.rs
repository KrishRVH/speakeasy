use super::*;
use anyhow::{anyhow, bail};
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
        CGEventTapPlacement, CGEventType, CallbackResult, EventField,
    },
    event_source::{CGEventSource, CGEventSourceStateID},
};
use objc2::{msg_send, runtime::AnyObject};
use std::{cell::RefCell, ffi::c_void, thread};

const OWN_INPUT: i64 = 0x53504541;
pub struct InputMonitor {
    control: Arc<MonitorControl<CFRunLoop>>,
    finished: async_channel::Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
    _observers: Vec<Observer>,
}

impl InputMonitor {
    pub fn start(tx: InputSender) -> anyhow::Result<Self> {
        let observers = lifecycle_observers(&tx);
        let control = Arc::new(MonitorControl::default());
        let native_control = control.clone();
        let (complete, finished) = async_channel::bounded(1);
        let thread = thread::Builder::new()
            .name("shortcut".into())
            .spawn(move || {
                if let Err(error) = run_monitor(&tx, &native_control)
                    && !native_control.stopping()
                    && !tx.is_closed()
                {
                    deliver(&tx, Input::Unavailable(error.to_string()));
                }
                native_control.clear();
                tx.close();
                let _ = complete.try_send(());
            })?;
        Ok(Self {
            control,
            finished,
            thread: Some(thread),
            _observers: observers,
        })
    }

    pub fn request_stop(&self) {
        self.control.request_stop(CFRunLoop::stop);
    }

    pub fn stopped(&self) -> impl std::future::Future<Output = ()> + use<> {
        let finished = self.finished.clone();
        async move {
            let _ = finished.recv().await;
        }
    }
}

struct RunLoopReady<'a> {
    control: &'a MonitorControl<CFRunLoop>,
    input: &'a InputSender,
}

extern "C" fn run_loop_ready(_: CFRunLoopObserverRef, _: CFRunLoopActivity, context: *mut c_void) {
    // SAFETY: run_monitor keeps this stack context alive until the run loop has
    // returned and removes its observer before releasing the context.
    let ready = unsafe { &*context.cast::<RunLoopReady<'_>>() };
    // Publish only once the run loop has entered. CFRunLoopStop before entry
    // does not reliably stop a subsequent run; an early stop is checked here.
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let run_loop = CFRunLoop::get_current();
        if !ready.control.start(run_loop.clone(), || {
            deliver(
                ready.input,
                Input::DesktopReady {
                    shortcut: SHORTCUT.into(),
                    cancel: "Escape".into(),
                },
            );
        }) {
            run_loop.stop();
        }
    }))
    .is_err()
    {
        ready.input.close();
        CFRunLoop::get_current().stop();
    }
}

fn run_monitor(tx: &InputSender, control: &MonitorControl<CFRunLoop>) -> anyhow::Result<()> {
    if control.stopping() {
        return Ok(());
    }
    let chord = RefCell::new(keyboard::Mac::default());
    let callback_input = tx.clone();
    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ],
        move |_, kind, event| {
            // Never unwind through the OS callback. A closed channel makes
            // the owner cancel rather than continue with lost edges.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                shortcut(&callback_input, &mut chord.borrow_mut(), kind, event)
            }))
            .unwrap_or_else(|_| {
                callback_input.close();
                CallbackResult::Keep
            })
        },
    )
    .map_err(|_| {
        anyhow!(
            "Allow Speakeasy in System Settings → Privacy & Security → Accessibility, then pause and resume dictation."
        )
    })?;
    let source = tap.mach_port().create_runloop_source(0).map_err(|_| {
        anyhow!("Cannot create shortcut run loop. Pause and resume dictation to try again.")
    })?;
    let run_loop = CFRunLoop::get_current();
    let mut ready = RunLoopReady { control, input: tx };
    let mut context = CFRunLoopObserverContext {
        version: 0,
        info: (&mut ready as *mut RunLoopReady<'_>).cast(),
        retain: None,
        release: None,
        copyDescription: None,
    };
    // SAFETY: this thread owns the observer and its stack context throughout
    // CFRunLoopRun. Entry fires once, and the observer is removed before return.
    let observer = unsafe {
        let raw = CFRunLoopObserverCreate(
            kCFAllocatorDefault,
            kCFRunLoopEntry,
            0,
            0,
            run_loop_ready,
            &mut context,
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
    control.clear();
    run_loop.remove_observer(&observer, modes);
    run_loop.remove_source(&source, modes);
    Ok(())
}

/// Fn, Fn+Space, and passive Escape, on the event tap's run-loop thread.
fn shortcut(
    tx: &InputSender,
    chord: &mut keyboard::Mac,
    kind: CGEventType,
    event: &CGEvent,
) -> CallbackResult {
    if matches!(
        kind,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        tx.close();
        return CallbackResult::Keep;
    }
    if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == OWN_INPUT {
        return CallbackResult::Keep;
    }
    if !matches!(
        kind,
        CGEventType::KeyDown | CGEventType::KeyUp | CGEventType::FlagsChanged
    ) {
        return CallbackResult::Keep;
    }
    let decision = chord.observe(
        event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
        matches!(kind, CGEventType::KeyDown),
        matches!(kind, CGEventType::FlagsChanged),
        event
            .get_flags()
            .contains(CGEventFlags::CGEventFlagSecondaryFn),
    );
    decision.deliver(tx);
    if decision.swallow {
        CallbackResult::Drop
    } else {
        CallbackResult::Keep
    }
}

impl Drop for InputMonitor {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn configure_pill(handle: RawWindowHandle) -> anyhow::Result<()> {
    let RawWindowHandle::AppKit(raw) = handle else {
        bail!("Expected an AppKit window");
    };
    // SAFETY: GPUI supplies a live NSView on the main thread. Its owning NSPanel
    // is retained by GPUI; these calls neither release nor store those pointers.
    unsafe {
        let view = raw.ns_view.as_ptr().cast::<AnyObject>();
        let window: *mut AnyObject = msg_send![view, window];
        if window.is_null() {
            bail!("The pill has no native window");
        }
        let _: () = msg_send![window, setIgnoresMouseEvents: true];
        // GPUI creates a titled panel. Its frame draws a highlight along the
        // top edge in dark mode, and its shadow outlines the whole transparent
        // window. A borderless panel has neither; the capsule draws its own
        // shadow. The content view already filled the frame, so its size holds.
        let _: () = msg_send![window, setStyleMask: 1_usize << 7]; // borderless, NSWindowStyleMaskNonactivatingPanel
        let _: () = msg_send![window, setHasShadow: false];
        let _: () = msg_send![window, setLevel: 3_isize]; // NSFloatingWindowLevel
        let _: () = msg_send![window, setCollectionBehavior: 1_usize | (1 << 8)]; // all Spaces + full-screen auxiliary
    }
    Ok(())
}

pub fn set_settings_visible(handle: RawWindowHandle, visible: bool) {
    if let RawWindowHandle::AppKit(raw) = handle {
        // SAFETY: GPUI owns this live NSView and NSWindow on the main thread.
        // The caller releases GPUI borrows before invoking AppKit.
        unsafe {
            let view = raw.ns_view.as_ptr().cast::<AnyObject>();
            let window: *mut AnyObject = msg_send![view, window];
            if visible {
                let _: () = msg_send![window, deminiaturize: std::ptr::null::<AnyObject>()];
                let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
            } else {
                let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
            }
        }
    }
}

pub fn set_pill_visible(handle: RawWindowHandle, visible: bool) {
    if let RawWindowHandle::AppKit(raw) = handle {
        // SAFETY: same main-thread NSView lifetime as configure_pill.
        unsafe {
            let view = raw.ns_view.as_ptr().cast::<AnyObject>();
            let window: *mut AnyObject = msg_send![view, window];
            if visible {
                if let Some(main) = objc2::MainThreadMarker::new() {
                    let pointer = objc2_app_kit::NSEvent::mouseLocation();
                    let screens = objc2_app_kit::NSScreen::screens(main);
                    if let Some(screen) = screens.iter().find(|screen| {
                        let frame = screen.frame();
                        pointer.x >= frame.origin.x
                            && pointer.x < frame.origin.x + frame.size.width
                            && pointer.y >= frame.origin.y
                            && pointer.y < frame.origin.y + frame.size.height
                    }) {
                        let frame = screen.visibleFrame();
                        let origin = objc2_foundation::NSPoint::new(
                            frame.origin.x + (frame.size.width - 400.0) / 2.0,
                            frame.origin.y + 2.0,
                        );
                        let _: () = msg_send![window, setFrameOrigin: origin];
                    }
                }
                let _: () = msg_send![window, orderFrontRegardless];
            } else {
                let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
            }
        }
    }
}

pub fn modifiers_down() -> bool {
    CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .and_then(CGEvent::new)
        .map(|event| {
            event.get_flags().intersects(
                CGEventFlags::CGEventFlagControl
                    | CGEventFlags::CGEventFlagAlternate
                    | CGEventFlags::CGEventFlagCommand
                    | CGEventFlags::CGEventFlagShift
                    | CGEventFlags::CGEventFlagSecondaryFn,
            )
        })
        .unwrap_or(true)
}

pub fn insert(
    text: &str,
    gate: &InsertPermit,
    preserve_clipboard: bool,
) -> anyhow::Result<Inserted> {
    if !gate.active() {
        return Ok(Inserted::Cancelled);
    }
    if preserve_clipboard {
        return insert_direct(text, gate);
    }
    let mut clipboard = arboard::Clipboard::new()?;
    if !gate.active() {
        return Ok(Inserted::Cancelled);
    }
    clipboard.set_text(text)?;
    let sequence = clipboard_sequence();
    if clipboard.get_text()? != text || clipboard_sequence() != sequence {
        return Ok(Inserted::Unavailable(
            "Clipboard changed. New contents were preserved; paste was not sent.",
        ));
    }
    if let Some(outcome) = insertion::preflight(
        has_external_target(),
        !modifiers_down(),
        insertion::Mode::Paste,
    ) {
        return Ok(outcome);
    }
    let source = CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| anyhow!("Cannot create keyboard event source"))?;
    let mut events = Vec::with_capacity(2);
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(source.clone(), 9, down)
            .map_err(|_| anyhow!("Cannot create paste event"))?;
        event.set_flags(CGEventFlags::CGEventFlagCommand);
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
        events.push(event);
    }
    if clipboard_sequence() != sequence {
        return Ok(Inserted::Unavailable(
            "Clipboard changed before paste. New contents were preserved.",
        ));
    }
    if !gate.commit() {
        return Ok(Inserted::Cancelled);
    }
    for event in events {
        event.post(CGEventTapLocation::HID);
    }
    Ok(Inserted::Sent)
}

pub fn reduced_motion() -> bool {
    // SAFETY: NSWorkspace is a process-owned singleton, queried on the main thread.
    unsafe {
        let workspace: *mut AnyObject = msg_send![objc2::class!(NSWorkspace), sharedWorkspace];
        msg_send![workspace, accessibilityDisplayShouldReduceMotion]
    }
}

fn clipboard_sequence() -> isize {
    // SAFETY: NSPasteboard generalPasteboard is process-owned and changeCount
    // is an integer getter; no native object ownership is transferred.
    unsafe {
        let board: *mut AnyObject = msg_send![objc2::class!(NSPasteboard), generalPasteboard];
        msg_send![board, changeCount]
    }
}

fn insert_direct(text: &str, gate: &InsertPermit) -> anyhow::Result<Inserted> {
    if let Some(outcome) = insertion::preflight(
        has_external_target(),
        !modifiers_down(),
        insertion::Mode::Direct,
    ) {
        return Ok(outcome);
    }
    let source = CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| anyhow!("Cannot create keyboard event source"))?;
    let mut events = Vec::new();
    let mut push = |chunk: &str| -> anyhow::Result<()> {
        let press = CGEvent::new_keyboard_event(source.clone(), 0, true)
            .map_err(|_| anyhow!("Cannot create text event"))?;
        press.set_string(chunk);
        let release = CGEvent::new_keyboard_event(source.clone(), 0, false)
            .map_err(|_| anyhow!("Cannot create text release"))?;
        for event in [press, release] {
            event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
            events.push(event);
        }
        Ok(())
    };
    // CoreGraphics unicode payloads have bounded size. Split on scalar values,
    // keeping surrogate pairs intact, and create every event before committing.
    let mut chunk = String::new();
    for ch in text.chars() {
        chunk.push(ch);
        if chunk.encode_utf16().count() >= 16 {
            push(&chunk)?;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        push(&chunk)?;
    }
    if !gate.commit() {
        return Ok(Inserted::Cancelled);
    }
    for event in events {
        event.post(CGEventTapLocation::HID);
    }
    Ok(Inserted::Sent)
}

struct Observer {
    center: objc2::rc::Retained<objc2_foundation::NSNotificationCenter>,
    token:
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>,
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
    use objc2_app_kit::{
        NSWorkspace, NSWorkspaceSessionDidResignActiveNotification,
        NSWorkspaceWillSleepNotification,
    };
    use objc2_foundation::{NSDistributedNotificationCenter, NSString};
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: AppKit's exported notification names are immutable constants.
    let names = unsafe {
        [
            NSWorkspaceWillSleepNotification,
            NSWorkspaceSessionDidResignActiveNotification,
        ]
    };
    let mut sources: Vec<_> = names
        .into_iter()
        .map(|name| (center.clone(), name.to_owned()))
        .collect();
    sources.push((
        NSDistributedNotificationCenter::defaultCenter().into_super(),
        NSString::from_str("com.apple.screenIsLocked"),
    ));
    sources
        .into_iter()
        .map(|(center, name)| {
            let input = input.clone();
            let callback = block2::RcBlock::new(
                move |_: std::ptr::NonNull<objc2_foundation::NSNotification>| {
                    deliver(&input, Input::Cancel);
                },
            );
            // SAFETY: the block captures a Send channel/atomic gate, accesses no
            // AppKit objects, and the center retains it until the observer is removed.
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(&name),
                    None,
                    None,
                    &callback,
                )
            };
            Observer { center, token }
        })
        .collect()
}

pub fn show_error(message: &str) {
    if let Some(main) = objc2::MainThreadMarker::new() {
        let alert = objc2_app_kit::NSAlert::new(main);
        alert.setMessageText(&objc2_foundation::NSString::from_str(
            "Speakeasy could not start",
        ));
        alert.setInformativeText(&objc2_foundation::NSString::from_str(message));
        alert.runModal();
    }
}

fn has_external_target() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .is_some_and(|app| app.processIdentifier() != std::process::id() as i32)
}
