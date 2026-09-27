use super::*;
use anyhow::{Context, anyhow, bail};
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use core_graphics::{
    event::{
        CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventType, CallbackResult, EventField,
    },
    event_source::{CGEventSource, CGEventSourceStateID},
};
use objc2::{msg_send, runtime::AnyObject};
use std::{cell::Cell, sync::mpsc, thread};

const OWN_INPUT: i64 = 0x53504541;
pub struct InputMonitor {
    run_loop: CFRunLoop,
    thread: Option<thread::JoinHandle<()>>,
    _observers: Vec<Observer>,
}

impl InputMonitor {
    pub fn start(tx: InputSender) -> anyhow::Result<Self> {
        let observers = lifecycle_observers(&tx);
        let (ready, started) = mpsc::sync_channel(1);
        let thread = thread::Builder::new().name("shortcut".into()).spawn(move || {
            let held = Cell::new(false);
            let tap = CGEventTap::new(CGEventTapLocation::Session, CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default, vec![CGEventType::KeyDown, CGEventType::KeyUp], move |_, kind, event| {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if matches!(kind, CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput) {
                            tx.close(); return CallbackResult::Keep;
                        }
                        if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == OWN_INPUT { return CallbackResult::Keep; }
                        let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                        if code == 53 && matches!(kind, CGEventType::KeyDown) { deliver(&tx, Input::Cancel); return CallbackResult::Keep; }
                        if code == 49 {
                            if matches!(kind, CGEventType::KeyDown) && held.get() && event.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT) == 0 {
                                held.set(false);
                                deliver(&tx, Input::Release);
                            }
                            let flags = event.get_flags();
                            let chord = flags.contains(CGEventFlags::CGEventFlagControl | CGEventFlags::CGEventFlagAlternate);
                            if matches!(kind, CGEventType::KeyDown) && (chord || held.get()) {
                                if !held.replace(true) { deliver(&tx, Input::Press); }
                                return CallbackResult::Drop;
                            }
                            if matches!(kind, CGEventType::KeyUp) && held.replace(false) {
                                deliver(&tx, Input::Release); return CallbackResult::Drop;
                            }
                        }
                        CallbackResult::Keep
                    }));
                    result.unwrap_or_else(|_| { tx.close(); CallbackResult::Keep })
                });
            let Ok(tap) = tap else { let _ = ready.send(Err("Allow Speakeasy in System Settings → Privacy & Security → Accessibility, then reopen it.")); return; };
            let source = match tap.mach_port().create_runloop_source(0) {
                Ok(source) => source,
                Err(_) => { let _ = ready.send(Err("Cannot create shortcut run loop")); return; }
            };
            let run_loop = CFRunLoop::get_current();
            // SAFETY: this is Core Foundation's permanent mode constant.
            run_loop.add_source(&source, unsafe { kCFRunLoopCommonModes });
            tap.enable();
            let _ = ready.send(Ok(run_loop));
            CFRunLoop::run_current();
        })?;
        let run_loop = started
            .recv()
            .context("Shortcut thread stopped")?
            .map_err(|message| anyhow!(message))?;
        Ok(Self {
            run_loop,
            thread: Some(thread),
            _observers: observers,
        })
    }
}

impl Drop for InputMonitor {
    fn drop(&mut self) {
        self.run_loop.stop();
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
                            frame.origin.y + 8.0,
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
                    | CGEventFlags::CGEventFlagShift,
            )
        })
        .unwrap_or(true)
}

pub fn insert(
    text: &str,
    gate: &InputSender,
    preserve_clipboard: bool,
) -> anyhow::Result<Inserted> {
    if !gate.active() {
        return Ok(Inserted::Cancelled);
    }
    if preserve_clipboard {
        return insert_direct(text, gate);
    }
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text)?;
    let sequence = clipboard_sequence();
    if clipboard.get_text()? != text || clipboard_sequence() != sequence {
        return Ok(Inserted::Unavailable(
            "Clipboard changed. New contents were preserved; paste was not sent.",
        ));
    }
    if !has_external_target() {
        return Ok(Inserted::Copied(
            "Copied. Focus an editor and press Command+V.",
        ));
    }
    if modifiers_down() {
        return Ok(Inserted::Copied(
            "Copied. Release the shortcut and press Command+V.",
        ));
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
        return Ok(Inserted::Unavailable("Clipboard changed before paste."));
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

fn insert_direct(text: &str, gate: &InputSender) -> anyhow::Result<Inserted> {
    if !has_external_target() {
        return Ok(Inserted::Unavailable(
            "Focus an editor and try again. Clipboard preserved.",
        ));
    }
    if modifiers_down() {
        return Ok(Inserted::Unavailable(
            "Release the shortcut and try again. Clipboard preserved.",
        ));
    }
    let source = CGEventSource::new(CGEventSourceStateID::Private)
        .map_err(|_| anyhow!("Cannot create keyboard event source"))?;
    let mut events = Vec::new();
    // CoreGraphics unicode payloads have bounded size. Split on scalar values,
    // keeping surrogate pairs intact, and create every event before committing.
    let mut chunk = String::new();
    for ch in text.chars() {
        chunk.push(ch);
        if chunk.encode_utf16().count() >= 16 {
            let event = CGEvent::new_keyboard_event(source.clone(), 0, true)
                .map_err(|_| anyhow!("Cannot create text event"))?;
            event.set_string(&chunk);
            event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
            events.push(event);
            let release = CGEvent::new_keyboard_event(source.clone(), 0, false)
                .map_err(|_| anyhow!("Cannot create text release"))?;
            release.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
            events.push(release);
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        let event = CGEvent::new_keyboard_event(source.clone(), 0, true)
            .map_err(|_| anyhow!("Cannot create text event"))?;
        event.set_string(&chunk);
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
        events.push(event);
        let release = CGEvent::new_keyboard_event(source, 0, false)
            .map_err(|_| anyhow!("Cannot create text release"))?;
        release.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, OWN_INPUT);
        events.push(release);
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
