//! Dynamically loaded libei sender. A missing library or missing modifier feedback is an
//! unavailable capability, never an assumed released keyboard.

use std::{
    ffi::c_void,
    fs::File,
    os::{
        fd::{BorrowedFd, IntoRawFd, OwnedFd, RawFd},
        unix::fs::FileExt,
    },
    ptr::{self, NonNull},
};

use anyhow::{Context, bail, ensure};
use libloading::Library;
use tokio::io::unix::AsyncFd;
use xkbcommon::xkb::{self, Keysym};

// Values from libei.h; they are ABI and must not be renumbered.
const EVENT_DISCONNECT: i32 = 2;
const EVENT_SEAT_ADDED: i32 = 3;
const EVENT_DEVICE_ADDED: i32 = 5;
const EVENT_DEVICE_REMOVED: i32 = 6;
const EVENT_DEVICE_PAUSED: i32 = 7;
const EVENT_DEVICE_RESUMED: i32 = 8;
const EVENT_KEYBOARD_MODIFIERS: i32 = 9;
const CAPABILITY_KEYBOARD: i32 = 1 << 2;
const CAPABILITY_TEXT: i32 = 1 << 6;
const KEYMAP_XKB: i32 = 1;

const MAX_KEYMAP_SIZE: usize = 16 * 1024 * 1024;
/// libei's limit on UTF-8 bytes in one text frame.
const MAX_TEXT_FRAME: usize = 254;
/// XKB keycodes are evdev keycodes offset by eight.
const XKB_KEYCODE_OFFSET: u32 = 8;

type Pointer = *mut c_void;
type Unref = unsafe extern "C" fn(Pointer) -> Pointer;
type ModifierQuery = unsafe extern "C" fn(Pointer) -> u32;
type TextInput = unsafe extern "C" fn(Pointer, *const u8, usize);

macro_rules! api {
    ($($name:ident: $kind:ty),+ $(,)?) => {
        struct Api { $($name: $kind,)+ library: Library }
        impl Api {
            fn load() -> anyhow::Result<Self> {
                // SAFETY: symbols and C signatures match libei's public ABI; the library stays
                // owned longer than every function pointer.
                unsafe {
                    let library = Library::new("libei.so.1").context("Install libei for Wayland input")?;
                    $(let $name = *library.get::<$kind>(concat!(stringify!($name), "\0").as_bytes())?;)+
                    Ok(Self { $($name,)+ library })
                }
            }
        }
    }
}

api! {
    ei_new_sender: unsafe extern "C" fn(Pointer) -> Pointer,
    ei_setup_backend_fd: unsafe extern "C" fn(Pointer, i32) -> i32,
    ei_get_fd: unsafe extern "C" fn(Pointer) -> i32,
    ei_dispatch: unsafe extern "C" fn(Pointer),
    ei_get_event: unsafe extern "C" fn(Pointer) -> Pointer,
    ei_event_get_type: unsafe extern "C" fn(Pointer) -> i32,
    ei_event_get_device: unsafe extern "C" fn(Pointer) -> Pointer,
    ei_event_get_seat: unsafe extern "C" fn(Pointer) -> Pointer,
    ei_event_unref: Unref,
    ei_unref: Unref,
    ei_seat_bind_capabilities: unsafe extern "C" fn(Pointer, ...),
    ei_seat_has_capability: unsafe extern "C" fn(Pointer, i32) -> bool,
    ei_device_has_capability: unsafe extern "C" fn(Pointer, i32) -> bool,
    ei_device_ref: unsafe extern "C" fn(Pointer) -> Pointer,
    ei_device_unref: Unref,
    ei_device_keyboard_get_keymap: unsafe extern "C" fn(Pointer) -> Pointer,
    ei_keymap_get_fd: unsafe extern "C" fn(Pointer) -> i32,
    ei_keymap_get_size: unsafe extern "C" fn(Pointer) -> usize,
    ei_keymap_get_type: unsafe extern "C" fn(Pointer) -> i32,
    ei_device_start_emulating: unsafe extern "C" fn(Pointer, u32),
    ei_device_keyboard_key: unsafe extern "C" fn(Pointer, u32, bool),
    ei_device_frame: unsafe extern "C" fn(Pointer, u64),
    ei_device_stop_emulating: unsafe extern "C" fn(Pointer),
    ei_now: unsafe extern "C" fn(Pointer) -> u64,
}

pub(super) struct Sender {
    // Declaration order is release order: device references, then the context, before the
    // library unloads.
    keyboard: Option<Device>,
    text: Option<Device>,
    context: EiContext,
    api: Api,
    keymap: Option<KeyboardMap>,
    keys: Option<PasteKeys>,
    modifiers: Modifiers,
    feedback: Option<ModifierFeedback>,
    utf8: Option<TextInput>,
    sequence: u32,
    readiness: AsyncFd<OwnedFd>,
}

impl Sender {
    pub(super) fn connect(fd: OwnedFd) -> anyhow::Result<Self> {
        let api = Api::load()?;
        let context = EiContext::new_sender(&api)?;
        // SAFETY: libei takes ownership of the descriptor, including its teardown.
        let status = unsafe { (api.ei_setup_backend_fd)(context.as_ptr(), fd.into_raw_fd()) };
        ensure!(status == 0, "Could not connect Wayland input sender");
        // SAFETY: the live context keeps its descriptor open while it is duplicated.
        let readiness = unsafe { register_readiness((api.ei_get_fd)(context.as_ptr())) }?;
        let feedback = ModifierFeedback::load(&api.library);
        // SAFETY: TextInput is the symbol's public C signature.
        let utf8 = unsafe {
            optional_symbol::<TextInput>(&api.library, b"ei_device_text_utf8_with_length\0")
        };
        Ok(Self {
            keyboard: None,
            text: None,
            context,
            api,
            keymap: None,
            keys: None,
            modifiers: Modifiers::Unknown,
            feedback,
            utf8,
            sequence: 0,
            readiness,
        })
    }

    pub(super) const fn readiness(&self) -> &AsyncFd<OwnedFd> {
        &self.readiness
    }

    /// Treats modifiers as unknown until libei reports them again.
    pub(super) fn forget_modifiers(&mut self) {
        self.modifiers = Modifiers::Unknown;
    }

    pub(super) fn can_paste(&self) -> bool {
        self.paste_target().is_some()
    }

    pub(super) fn can_type_text(&self) -> bool {
        self.text_target().is_some()
    }

    /// The resumed keyboard and its paste keys, once its modifiers are known to be released.
    fn paste_target(&self) -> Option<(Pointer, PasteKeys)> {
        if self.modifiers != Modifiers::Released {
            return None;
        }
        let keyboard = self.keyboard.as_ref().filter(|keyboard| keyboard.resumed)?;
        Some((keyboard.as_ptr(), self.keys?))
    }

    /// The UTF-8 entry point and the resumed text device it writes to.
    fn text_target(&self) -> Option<(TextInput, Pointer)> {
        let device = self.text.as_ref().filter(|device| device.resumed)?;
        Some((self.utf8?, device.as_ptr()))
    }

    pub(super) fn dispatch(&mut self) -> anyhow::Result<()> {
        // SAFETY: the context is live and dispatched only on its owning thread.
        unsafe { (self.api.ei_dispatch)(self.context.as_ptr()) };
        while let Some(event) = self.next_event() {
            match event.kind {
                EVENT_DISCONNECT => bail!("Wayland input permission was disconnected"),
                EVENT_SEAT_ADDED => self.bind_seat(&event),
                EVENT_DEVICE_ADDED => self.add_device(&event),
                EVENT_DEVICE_REMOVED | EVENT_DEVICE_PAUSED => self.set_resumed(event.device, false),
                EVENT_DEVICE_RESUMED => self.set_resumed(event.device, true),
                EVENT_KEYBOARD_MODIFIERS if self.is_keyboard(event.device) => {
                    self.update_modifiers(&event);
                },
                _ => {},
            }
        }
        Ok(())
    }

    fn next_event(&self) -> Option<Event> {
        // SAFETY: the context is live; a returned event stays live until its guard drops.
        unsafe {
            let raw = NonNull::new((self.api.ei_get_event)(self.context.as_ptr()))?;
            Some(Event {
                raw,
                kind: (self.api.ei_event_get_type)(raw.as_ptr()),
                device: NonNull::new((self.api.ei_event_get_device)(raw.as_ptr())),
                unref: self.api.ei_event_unref,
            })
        }
    }

    fn bind_seat(&self, event: &Event) {
        // SAFETY: the live event keeps its seat alive; each capability list ends with null.
        unsafe {
            let seat = (self.api.ei_event_get_seat)(event.raw.as_ptr());
            if seat.is_null() || !(self.api.ei_seat_has_capability)(seat, CAPABILITY_KEYBOARD) {
                return;
            }
            if self.utf8.is_some() && (self.api.ei_seat_has_capability)(seat, CAPABILITY_TEXT) {
                (self.api.ei_seat_bind_capabilities)(
                    seat,
                    CAPABILITY_KEYBOARD,
                    CAPABILITY_TEXT,
                    ptr::null::<c_void>(),
                );
            } else {
                (self.api.ei_seat_bind_capabilities)(
                    seat,
                    CAPABILITY_KEYBOARD,
                    ptr::null::<c_void>(),
                );
            }
        }
    }

    fn add_device(&mut self, event: &Event) {
        let Some(device) = event.device else {
            return;
        };
        // SAFETY: the live event keeps its device alive while it is queried, referenced, and its
        // keymap read.
        unsafe {
            if (self.api.ei_device_has_capability)(device.as_ptr(), CAPABILITY_KEYBOARD) {
                self.keyboard = Some(Device::acquire(&self.api, device));
                self.modifiers = Modifiers::Unknown;
                self.keymap = device_keymap(&self.api, device.as_ptr()).ok();
                self.keys = None;
            }
            if (self.api.ei_device_has_capability)(device.as_ptr(), CAPABILITY_TEXT) {
                self.text = Some(Device::acquire(&self.api, device));
            }
        }
    }

    fn is_keyboard(&self, device: Option<NonNull<c_void>>) -> bool {
        self.keyboard
            .as_ref()
            .is_some_and(|keyboard| keyboard.is(device))
    }

    fn set_resumed(&mut self, device: Option<NonNull<c_void>>, resumed: bool) {
        if let Some(keyboard) = self
            .keyboard
            .as_mut()
            .filter(|keyboard| keyboard.is(device))
        {
            keyboard.resumed = resumed;
            self.modifiers = Modifiers::Unknown;
        }
        if let Some(text) = self.text.as_mut().filter(|text| text.is(device)) {
            text.resumed = resumed;
        }
    }

    fn update_modifiers(&mut self, event: &Event) {
        let (Some(feedback), Some(keymap)) = (&self.feedback, &self.keymap) else {
            return;
        };
        let raw = event.raw.as_ptr();
        // SAFETY: the live event reports keyboard modifiers, and the library stays loaded.
        let (group, active_mask) = unsafe {
            (
                (feedback.group)(raw),
                (feedback.depressed)(raw) | (feedback.latched)(raw) | (feedback.locked)(raw),
            )
        };
        self.keys = keymap.paste_keys(group);
        self.modifiers = if active_mask & keymap.paste_blockers == 0 {
            Modifiers::Released
        } else {
            Modifiers::Held
        };
    }

    pub(super) fn paste(&mut self, terminal: bool) -> anyhow::Result<()> {
        let (keyboard, keys) = self
            .paste_target()
            .context("Wayland keyboard is not ready to paste")?;
        self.sequence = self.sequence.wrapping_add(1);
        // SAFETY: keyboard is a held, resumed keyboard-capable device. The complete sequence
        // releases every injected key before stopping.
        unsafe {
            (self.api.ei_device_start_emulating)(keyboard, self.sequence);
            (self.api.ei_device_keyboard_key)(keyboard, keys.control, true);
            if terminal {
                (self.api.ei_device_keyboard_key)(keyboard, keys.shift, true);
            }
            (self.api.ei_device_keyboard_key)(keyboard, keys.v, true);
            (self.api.ei_device_frame)(keyboard, (self.api.ei_now)(self.context.as_ptr()));
            (self.api.ei_device_keyboard_key)(keyboard, keys.v, false);
            if terminal {
                (self.api.ei_device_keyboard_key)(keyboard, keys.shift, false);
            }
            (self.api.ei_device_keyboard_key)(keyboard, keys.control, false);
            (self.api.ei_device_frame)(keyboard, (self.api.ei_now)(self.context.as_ptr()));
            (self.api.ei_device_stop_emulating)(keyboard);
        }
        Ok(())
    }

    pub(super) fn type_text(&mut self, text: &str) -> anyhow::Result<()> {
        let (send, device) = self
            .text_target()
            .context("Direct UTF-8 input is unavailable on this desktop")?;
        ensure!(
            !text.contains('\0'),
            "Direct input cannot contain a null character"
        );
        self.sequence = self.sequence.wrapping_add(1);
        // SAFETY: device is a held, resumed text-capable device. Each chunk stays alive for its C
        // call, and each frame respects the EI UTF-8 frame limit.
        unsafe {
            (self.api.ei_device_start_emulating)(device, self.sequence);
            for chunk in utf8_chunks(text) {
                send(device, chunk.as_ptr(), chunk.len());
                (self.api.ei_device_frame)(device, (self.api.ei_now)(self.context.as_ptr()));
            }
            (self.api.ei_device_stop_emulating)(device);
        }
        Ok(())
    }
}

/// Whether a paste-blocking modifier is held, as libei last reported.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Modifiers {
    Unknown,
    Held,
    Released,
}

/// The XKB modifier state libei reports with keyboard-modifier events.
struct ModifierFeedback {
    depressed: ModifierQuery,
    latched: ModifierQuery,
    locked: ModifierQuery,
    group: ModifierQuery,
}

impl ModifierFeedback {
    fn load(library: &Library) -> Option<Self> {
        // SAFETY: ModifierQuery is each symbol's public C signature.
        let (depressed, latched, locked, group) = unsafe {
            (
                optional_symbol::<ModifierQuery>(
                    library,
                    b"ei_event_keyboard_get_xkb_mods_depressed\0",
                ),
                optional_symbol::<ModifierQuery>(
                    library,
                    b"ei_event_keyboard_get_xkb_mods_latched\0",
                ),
                optional_symbol::<ModifierQuery>(
                    library,
                    b"ei_event_keyboard_get_xkb_mods_locked\0",
                ),
                optional_symbol::<ModifierQuery>(library, b"ei_event_keyboard_get_xkb_group\0"),
            )
        };
        Some(Self {
            depressed: depressed?,
            latched: latched?,
            locked: locked?,
            group: group?,
        })
    }
}

/// The sender's owning reference to its libei context.
struct EiContext {
    raw: NonNull<c_void>,
    unref: Unref,
}

impl EiContext {
    fn new_sender(api: &Api) -> anyhow::Result<Self> {
        // SAFETY: a sender context is created without callback userdata.
        let raw = unsafe { (api.ei_new_sender)(ptr::null_mut()) };
        Ok(Self {
            raw: NonNull::new(raw).context("Could not create Wayland input sender")?,
            unref: api.ei_unref,
        })
    }

    const fn as_ptr(&self) -> Pointer {
        self.raw.as_ptr()
    }
}

impl Drop for EiContext {
    fn drop(&mut self) {
        // SAFETY: this is the context's only owning reference, and every owner drops it before
        // unloading the library; dropping it closes the EIS descriptor.
        unsafe { (self.unref)(self.raw.as_ptr()) };
    }
}

/// A device reference the sender holds, released when dropped.
struct Device {
    raw: NonNull<c_void>,
    /// Whether libei has resumed the device, so it accepts emulated input.
    resumed: bool,
    unref: Unref,
}

impl Device {
    /// References `device`, which stays paused until libei resumes it.
    ///
    /// # Safety
    /// `device` must be a live libei device.
    unsafe fn acquire(api: &Api, device: NonNull<c_void>) -> Self {
        // SAFETY: the caller keeps the device live while this reference is taken.
        unsafe { (api.ei_device_ref)(device.as_ptr()) };
        Self {
            raw: device,
            resumed: false,
            unref: api.ei_device_unref,
        }
    }

    fn is(&self, device: Option<NonNull<c_void>>) -> bool {
        device == Some(self.raw)
    }

    const fn as_ptr(&self) -> Pointer {
        self.raw.as_ptr()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the guard holds this reference, and the sender releases its devices before its
        // context and library.
        unsafe { (self.unref)(self.raw.as_ptr()) };
    }
}

/// One queued libei event, released when dropped.
struct Event {
    raw: NonNull<c_void>,
    kind: i32,
    device: Option<NonNull<c_void>>,
    unref: Unref,
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the guard holds the event's only reference, and the sender that dispatched it
        // keeps the library loaded.
        unsafe { (self.unref)(self.raw.as_ptr()) };
    }
}

struct KeyboardMap {
    keymap: xkb::Keymap,
    paste_blockers: u32,
}

impl KeyboardMap {
    fn new(keymap: xkb::Keymap) -> anyhow::Result<Self> {
        // Omits Lock (Caps) and Mod2 (Num Lock): locked modifiers count as active, yet they do not
        // change Ctrl+V, so blocking on them would disable paste while either lock is on.
        let mut paste_blockers = 0;
        for name in ["Shift", "Control", "Mod1", "Mod3", "Mod4", "Mod5"] {
            let index = keymap.mod_get_index(name);
            if index < u32::BITS {
                paste_blockers |= 1 << index;
            }
        }
        ensure!(paste_blockers != 0, "Missing paste modifier mapping");
        Ok(Self {
            keymap,
            paste_blockers,
        })
    }

    fn paste_keys(&self, group: u32) -> Option<PasteKeys> {
        let mut control = None;
        let mut shift = None;
        let mut v = None;
        self.keymap.key_for_each(|keymap, key| {
            let Some(layout) = group.checked_rem(keymap.num_layouts_for_key(key)) else {
                return;
            };
            let symbols = keymap.key_get_syms_by_level(key, layout, 0);
            let Some(code) = key.raw().checked_sub(XKB_KEYCODE_OFFSET) else {
                return;
            };
            if symbols.contains(&Keysym::Control_L) {
                control = Some(code);
            }
            if symbols.contains(&Keysym::Shift_L) {
                shift = Some(code);
            }
            if symbols.contains(&Keysym::v) {
                v = Some(code);
            }
        });
        Some(PasteKeys {
            control: control?,
            shift: shift?,
            v: v?,
        })
    }
}

#[derive(Clone, Copy)]
struct PasteKeys {
    control: u32,
    shift: u32,
    v: u32,
}

/// Looks up an optional libei symbol; absence means the capability is unavailable.
///
/// # Safety
/// `T` must be the symbol's C signature, and the returned function pointer must not be called
/// after `library` unloads.
unsafe fn optional_symbol<T: Copy>(library: &Library, name: &[u8]) -> Option<T> {
    // SAFETY: the caller supplies the symbol's signature.
    unsafe { library.get::<T>(name) }.ok().map(|symbol| *symbol)
}

/// Registers a duplicate of `fd` for readiness polling.
///
/// # Safety
/// A non-negative `fd` must stay open until this returns.
unsafe fn register_readiness(fd: RawFd) -> anyhow::Result<AsyncFd<OwnedFd>> {
    ensure!(fd >= 0, "Desktop connection has no valid file descriptor");
    // SAFETY: the caller keeps fd open; only the duplicate is owned and later closed.
    let duplicate = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
    Ok(AsyncFd::new(duplicate)?)
}

/// Compiles the XKB keymap libei advertises for `device`.
///
/// # Safety
/// `device` must be a live keyboard-capable libei device.
unsafe fn device_keymap(api: &Api, device: Pointer) -> anyhow::Result<KeyboardMap> {
    // SAFETY: the caller keeps the device live, so its borrowed keymap and descriptor are too.
    let (fd, size) = unsafe {
        let keymap = (api.ei_device_keyboard_get_keymap)(device);
        ensure!(
            !keymap.is_null() && (api.ei_keymap_get_type)(keymap) == KEYMAP_XKB,
            "No XKB keymap"
        );
        (
            (api.ei_keymap_get_fd)(keymap),
            (api.ei_keymap_get_size)(keymap),
        )
    };
    ensure!(
        fd >= 0 && size > 0 && size <= MAX_KEYMAP_SIZE,
        "Invalid XKB keymap size"
    );
    // SAFETY: fd is borrowed from the live keymap; only its duplicate is owned.
    let file = File::from(unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?);
    let mut bytes = vec![0; size];
    file.read_exact_at(&mut bytes, 0)?;
    let mut source = String::from_utf8(bytes)?;
    source.truncate(source.trim_end_matches('\0').len());
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_string(
        &context,
        source,
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .context("Invalid XKB keymap")?;
    KeyboardMap::new(keymap)
}

fn utf8_chunks(mut text: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if text.is_empty() {
            return None;
        }
        let (chunk, rest) = text.split_at(text.floor_char_boundary(MAX_TEXT_FRAME));
        text = rest;
        Some(chunk)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_uses_the_advertised_keyboard_layout() -> anyhow::Result<()> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        for (layout, variant, group, expected) in [
            ("us", "", 0, 47),
            ("us", "dvorak", 0, 52),
            ("us,us", ",dvorak", 1, 52),
        ] {
            let keymap = xkb::Keymap::new_from_names(
                &context,
                "evdev",
                "pc105",
                layout,
                variant,
                None,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
            .context("Fixture keymap")?;
            let map = KeyboardMap::new(keymap)?;
            let keys = map.paste_keys(group).context("Fixture paste binding")?;
            assert_eq!((keys.control, keys.shift, keys.v), (29, 42, expected));
        }
        Ok(())
    }

    #[test]
    fn text_frames_preserve_unicode_and_protocol_length() {
        let text = "é🙂λ".repeat(100);
        let chunks: Vec<_> = utf8_chunks(&text).collect();
        assert!(
            chunks
                .iter()
                .all(|chunk| !chunk.is_empty() && chunk.len() <= MAX_TEXT_FRAME)
        );
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn invalid_native_descriptor_is_reported_before_borrowing_it() {
        // SAFETY: a negative descriptor is rejected before it is borrowed.
        assert!(unsafe { register_readiness(-1) }.is_err());
    }
}
