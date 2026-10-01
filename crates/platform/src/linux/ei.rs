//! Dynamically loaded libei sender. A missing library or missing modifier
//! feedback is an unavailable capability, never an assumed released keyboard.
use anyhow::{Context, ensure};
use libloading::Library;
use std::{
    ffi::c_void,
    os::fd::{BorrowedFd, IntoRawFd, OwnedFd},
    os::unix::fs::FileExt,
};
use tokio::io::unix::AsyncFd;
type Pointer = *mut c_void;
type GetMods = unsafe extern "C" fn(Pointer) -> u32;
macro_rules! api {
    ($($name:ident: $kind:ty),+ $(,)?) => {
        struct Api { $($name: $kind,)+ _library: Library }
        impl Api {
            fn load() -> anyhow::Result<Self> {
                // SAFETY: symbols and C signatures match libei's public ABI;
                // the library stays owned longer than every function pointer.
                unsafe {
                    let library = Library::new("libei.so.1").context("Install libei for Wayland input")?;
                    $(let $name = *library.get::<$kind>(concat!(stringify!($name), "\0").as_bytes())?;)+
                    Ok(Self { $($name,)+ _library: library })
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
    ei_event_unref: unsafe extern "C" fn(Pointer) -> Pointer,
    ei_unref: unsafe extern "C" fn(Pointer) -> Pointer,
    ei_seat_bind_capabilities: unsafe extern "C" fn(Pointer, ...),
    ei_seat_has_capability: unsafe extern "C" fn(Pointer, i32) -> bool,
    ei_device_has_capability: unsafe extern "C" fn(Pointer, i32) -> bool,
    ei_device_ref: unsafe extern "C" fn(Pointer) -> Pointer,
    ei_device_unref: unsafe extern "C" fn(Pointer) -> Pointer,
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Modifiers {
    Unknown,
    Held,
    Released,
}
pub(super) struct Sender {
    api: Api,
    context: Pointer,
    keyboard: Pointer,
    text: Pointer,
    keyboard_ready: bool,
    text_ready: bool,
    map: Option<KeyboardMap>,
    keys: Option<PasteKeys>,
    modifiers: Modifiers,
    feedback: Option<[GetMods; 4]>,
    utf8: Option<unsafe extern "C" fn(Pointer, *const u8, usize)>,
    sequence: u32,
    pub ready: AsyncFd<OwnedFd>,
}
impl Sender {
    pub(super) fn connect(fd: OwnedFd) -> anyhow::Result<Self> {
        let api = Api::load()?;
        // SAFETY: constructing an owned libei context with no callback userdata.
        let context = unsafe { (api.ei_new_sender)(std::ptr::null_mut()) };
        ensure!(!context.is_null(), "Could not create Wayland input sender");
        // SAFETY: libei takes ownership of this owned descriptor, including
        // teardown; context is valid until Sender::drop.
        let status = unsafe { (api.ei_setup_backend_fd)(context, fd.into_raw_fd()) };
        if status != 0 {
            // SAFETY: this is the sole owning reference to the context.
            unsafe {
                (api.ei_unref)(context);
            }
            anyhow::bail!("Could not connect Wayland input sender");
        }
        // SAFETY: the libei connection remains alive while its fd is duplicated.
        let ready = match super::x11::readiness(unsafe { (api.ei_get_fd)(context) }) {
            Ok(ready) => ready,
            Err(error) => {
                // SAFETY: release the sole owner on failed initialization.
                unsafe {
                    (api.ei_unref)(context);
                }
                return Err(error);
            },
        };
        // SAFETY: these optional symbols use the public ABI; absence means no
        // usable capability. Api owns the library throughout their use.
        let (feedback, utf8) = unsafe {
            let load = |name: &[u8]| api._library.get::<GetMods>(name).ok().map(|symbol| *symbol);
            let feedback = load(b"ei_event_keyboard_get_xkb_mods_depressed\0")
                .zip(load(b"ei_event_keyboard_get_xkb_mods_latched\0"))
                .zip(load(b"ei_event_keyboard_get_xkb_mods_locked\0"))
                .zip(load(b"ei_event_keyboard_get_xkb_group\0"))
                .map(|(((depressed, latched), locked), group)| [depressed, latched, locked, group]);
            let utf8 = api
                ._library
                .get::<unsafe extern "C" fn(Pointer, *const u8, usize)>(
                    b"ei_device_text_utf8_with_length\0",
                )
                .ok()
                .map(|symbol| *symbol);
            (feedback, utf8)
        };
        Ok(Self {
            api,
            context,
            keyboard: std::ptr::null_mut(),
            text: std::ptr::null_mut(),
            keyboard_ready: false,
            text_ready: false,
            map: None,
            keys: None,
            modifiers: Modifiers::Unknown,
            feedback,
            utf8,
            sequence: 0,
            ready,
        })
    }
    pub(super) fn invalidate(&mut self) {
        self.modifiers = Modifiers::Unknown;
    }
    pub(super) fn modifiers(&self) -> Modifiers {
        self.modifiers
    }
    pub(super) fn text_available(&self) -> bool {
        self.text_ready && self.utf8.is_some()
    }
    pub(super) fn keyboard_available(&self) -> bool {
        self.keyboard_ready && self.keys.is_some()
    }
    pub(super) fn dispatch(&mut self) -> anyhow::Result<()> {
        // SAFETY: only this owned desktop thread accesses the context/devices.
        // Every returned event is unreferenced before leaving this method.
        unsafe {
            (self.api.ei_dispatch)(self.context);
            loop {
                let event = (self.api.ei_get_event)(self.context);
                if event.is_null() {
                    break;
                }
                let kind = (self.api.ei_event_get_type)(event);
                let device = (self.api.ei_event_get_device)(event);
                match kind {
                    2 => {
                        (self.api.ei_event_unref)(event);
                        anyhow::bail!("Wayland input permission was disconnected");
                    },
                    3 => {
                        let seat = (self.api.ei_event_get_seat)(event);
                        if !seat.is_null() && (self.api.ei_seat_has_capability)(seat, 4) {
                            if self.utf8.is_some() && (self.api.ei_seat_has_capability)(seat, 64) {
                                (self.api.ei_seat_bind_capabilities)(
                                    seat,
                                    4_i32,
                                    64_i32,
                                    std::ptr::null::<c_void>(),
                                );
                            } else {
                                (self.api.ei_seat_bind_capabilities)(
                                    seat,
                                    4_i32,
                                    std::ptr::null::<c_void>(),
                                );
                            }
                        }
                    },
                    5 if !device.is_null() => {
                        if (self.api.ei_device_has_capability)(device, 4) {
                            if !self.keyboard.is_null() {
                                (self.api.ei_device_unref)(self.keyboard);
                            }
                            self.keyboard = (self.api.ei_device_ref)(device);
                            self.keyboard_ready = false;
                            self.modifiers = Modifiers::Unknown;
                            self.map = keymap(&self.api, device).ok();
                            self.keys = None;
                        }
                        if (self.api.ei_device_has_capability)(device, 64) {
                            if !self.text.is_null() {
                                (self.api.ei_device_unref)(self.text);
                            }
                            self.text = (self.api.ei_device_ref)(device);
                            self.text_ready = false;
                        }
                    },
                    6 | 7 => {
                        if device == self.keyboard {
                            self.keyboard_ready = false;
                            self.modifiers = Modifiers::Unknown;
                        }
                        if device == self.text {
                            self.text_ready = false;
                        }
                    },
                    8 => {
                        if device == self.keyboard {
                            self.keyboard_ready = true;
                            self.modifiers = Modifiers::Unknown;
                        }
                        if device == self.text {
                            self.text_ready = true;
                        }
                    },
                    9 if device == self.keyboard => {
                        if let (Some(get), Some(map)) = (self.feedback, self.map.as_ref()) {
                            self.keys = map.paste_keys(get[3](event));
                            let bits = get[0](event) | get[1](event) | get[2](event);
                            self.modifiers = if bits & map.mask == 0 {
                                Modifiers::Released
                            } else {
                                Modifiers::Held
                            };
                        }
                    },
                    _ => {},
                }
                (self.api.ei_event_unref)(event);
            }
        }
        Ok(())
    }
    pub(super) fn paste(&mut self, terminal: bool) -> anyhow::Result<()> {
        ensure!(
            self.keyboard_ready && self.modifiers == Modifiers::Released,
            "Wayland modifier state is unavailable"
        );
        let keys = self
            .keys
            .context("The current keyboard layout has no paste binding")?;
        self.sequence = self.sequence.wrapping_add(1);
        // SAFETY: keyboard is an owned, resumed keyboard-capable device. The
        // complete sequence releases every injected key before stopping.
        unsafe {
            (self.api.ei_device_start_emulating)(self.keyboard, self.sequence);
            (self.api.ei_device_keyboard_key)(self.keyboard, keys.control, true);
            if terminal {
                (self.api.ei_device_keyboard_key)(self.keyboard, keys.shift, true);
            }
            (self.api.ei_device_keyboard_key)(self.keyboard, keys.v, true);
            (self.api.ei_device_frame)(self.keyboard, (self.api.ei_now)(self.context));
            (self.api.ei_device_keyboard_key)(self.keyboard, keys.v, false);
            if terminal {
                (self.api.ei_device_keyboard_key)(self.keyboard, keys.shift, false);
            }
            (self.api.ei_device_keyboard_key)(self.keyboard, keys.control, false);
            (self.api.ei_device_frame)(self.keyboard, (self.api.ei_now)(self.context));
            (self.api.ei_device_stop_emulating)(self.keyboard);
        }
        Ok(())
    }
    pub(super) fn text(&mut self, text: &str) -> anyhow::Result<()> {
        ensure!(
            self.text_available(),
            "Direct UTF-8 input is unavailable on this desktop"
        );
        ensure!(
            !text.contains('\0'),
            "Direct input cannot contain a null character"
        );
        let send = self.utf8.context("Direct UTF-8 input is unavailable")?;
        self.sequence = self.sequence.wrapping_add(1);
        // SAFETY: text is a resumed text-capable device. Each buffer remains
        // alive for the C call, each frame respects the EI 254-byte UTF-8 limit.
        unsafe {
            (self.api.ei_device_start_emulating)(self.text, self.sequence);
            for chunk in utf8_chunks(text) {
                send(self.text, chunk.as_ptr(), chunk.len());
                (self.api.ei_device_frame)(self.text, (self.api.ei_now)(self.context));
            }
            (self.api.ei_device_stop_emulating)(self.text);
        }
        Ok(())
    }
}
fn utf8_chunks(mut text: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if text.is_empty() {
            return None;
        }
        let (chunk, rest) = text.split_at(text.floor_char_boundary(254));
        text = rest;
        Some(chunk)
    })
}
unsafe fn keymap(api: &Api, device: Pointer) -> anyhow::Result<KeyboardMap> {
    // SAFETY: callers supply a valid owned keyboard device; the borrowed keymap
    // and its fd remain alive while copied using positional reads.
    let (fd, size) = unsafe {
        let map = (api.ei_device_keyboard_get_keymap)(device);
        ensure!(
            !map.is_null() && (api.ei_keymap_get_type)(map) == 1,
            "No XKB keymap"
        );
        ((api.ei_keymap_get_fd)(map), (api.ei_keymap_get_size)(map))
    };
    ensure!(
        fd >= 0 && size > 0 && size <= 16 * 1024 * 1024,
        "Invalid XKB keymap size"
    );
    // SAFETY: fd is borrowed from the live device; only its duplicate is owned.
    let file = std::fs::File::from(unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?);
    let mut bytes = vec![0; size];
    file.read_exact_at(&mut bytes, 0)?;
    let source = String::from_utf8(bytes)?.trim_end_matches('\0').to_owned();
    let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
    let map = xkbcommon::xkb::Keymap::new_from_string(
        &context,
        source,
        xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1,
        xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .context("Invalid XKB keymap")?;
    KeyboardMap::new(map)
}
struct KeyboardMap {
    map: xkbcommon::xkb::Keymap,
    mask: u32,
}
#[derive(Clone, Copy)]
struct PasteKeys {
    control: u32,
    shift: u32,
    v: u32,
}
impl KeyboardMap {
    fn new(map: xkbcommon::xkb::Keymap) -> anyhow::Result<Self> {
        let mut mask = 0;
        for name in ["Shift", "Control", "Mod1", "Mod3", "Mod4", "Mod5"] {
            let index = map.mod_get_index(name);
            if index < 32 {
                mask |= 1 << index;
            }
        }
        ensure!(mask != 0, "Missing paste modifier mapping");
        Ok(Self { map, mask })
    }
    fn paste_keys(&self, group: u32) -> Option<PasteKeys> {
        let symbol = |name| xkbcommon::xkb::keysym_from_name(name, xkbcommon::xkb::KEYSYM_NO_FLAGS);
        let mut control = None;
        let mut shift = None;
        let mut v = None;
        self.map.key_for_each(|map, key| {
            let Some(layout) = group.checked_rem(map.num_layouts_for_key(key)) else {
                return;
            };
            let syms = map.key_get_syms_by_level(key, layout, 0);
            let Some(code) = key.raw().checked_sub(8) else {
                return;
            };
            if syms.contains(&symbol("Control_L")) {
                control = Some(code);
            }
            if syms.contains(&symbol("Shift_L")) {
                shift = Some(code);
            }
            if syms.contains(&symbol("v")) {
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
impl Drop for Sender {
    fn drop(&mut self) {
        // SAFETY: these are the owning references acquired above; dropping the
        // context closes its EIS fd before the dynamic library is unloaded.
        unsafe {
            if !self.keyboard.is_null() {
                (self.api.ei_device_unref)(self.keyboard);
            }
            if !self.text.is_null() {
                (self.api.ei_device_unref)(self.text);
            }
            (self.api.ei_unref)(self.context);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paste_uses_the_advertised_keyboard_layout() -> anyhow::Result<()> {
        let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
        for (layout, variant, group, expected) in [
            ("us", "", 0, 47),
            ("us", "dvorak", 0, 52),
            ("us,us", ",dvorak", 1, 52),
        ] {
            let map = xkbcommon::xkb::Keymap::new_from_names(
                &context,
                "evdev",
                "pc105",
                layout,
                variant,
                None,
                xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
            .context("Fixture keymap")?;
            let map = KeyboardMap::new(map)?;
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
                .all(|chunk| !chunk.is_empty() && chunk.len() <= 254)
        );
        assert_eq!(chunks.concat(), text);
    }
}
