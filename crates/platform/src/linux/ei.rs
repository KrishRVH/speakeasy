use std::{
    ffi::c_void,
    fs::File,
    os::{
        fd::{BorrowedFd, IntoRawFd, OwnedFd, RawFd},
        unix::fs::FileExt,
    },
    ptr::{self, NonNull},
};

use anyhow::{Context as _, bail, ensure};
use libloading::os::unix::{Library, RTLD_LAZY, RTLD_LOCAL};
use tokio::io::unix::AsyncFd;
use xkbcommon::xkb::{self, Keysym};

use super::{PasteChord, PasteKeys};

const EVENT_DISCONNECT: i32 = 2;
const EVENT_SEAT_ADDED: i32 = 3;
const EVENT_DEVICE_ADDED: i32 = 5;
const EVENT_DEVICE_REMOVED: i32 = 6;
const EVENT_DEVICE_PAUSED: i32 = 7;
const EVENT_DEVICE_RESUMED: i32 = 8;
const EVENT_KEYBOARD_MODIFIERS: i32 = 9;
const EVENT_PONG: i32 = 90;
const CAPABILITY_KEYBOARD: i32 = 1 << 2;
const CAPABILITY_TEXT: i32 = 1 << 6;
const KEYMAP_XKB: i32 = 1;

const MAX_KEYMAP_SIZE: usize = 16 * 1024 * 1024;

const MAX_TEXT_FRAME: usize = 254;

const XKB_KEYCODE_OFFSET: u32 = 8;

#[repr(C)]
struct Ei([u8; 0]);
#[repr(C)]
struct EiEvent([u8; 0]);
#[repr(C)]
struct EiSeat([u8; 0]);
#[repr(C)]
struct EiDevice([u8; 0]);
#[repr(C)]
struct EiKeymap([u8; 0]);
#[repr(C)]
struct EiPing([u8; 0]);

type Unref<T> = unsafe extern "C" fn(*mut T) -> *mut T;
type TextInput = unsafe extern "C" fn(*mut EiDevice, *const u8, usize);

macro_rules! api {
    ($($name:ident: $kind:ty),+ $(,)?) => {
        pub(super) struct Api { $($name: $kind,)+ text_utf8: Option<TextInput>, ping: Option<PingApi> }
        impl Api {
            pub(super) fn load() -> anyhow::Result<Self> {
                unsafe {
                    let library = Library::open(
                        Some("libei.so.1"),
                        RTLD_LAZY | RTLD_LOCAL | libc::RTLD_NODELETE,
                    )
                    .context("Install libei for Wayland input")?;
                    $(let $name = *library.get::<$kind>(concat!(stringify!($name), "\0").as_bytes())?;)+
                    let text_utf8 = library
                        .get::<TextInput>(b"ei_device_text_utf8_with_length\0")
                        .ok()
                        .map(|symbol| *symbol);
                    let ping = PingApi::load(&library);
                    Ok(Self { $($name,)+ text_utf8, ping })
                }
            }
        }
    }
}

api! {
    ei_new_sender: unsafe extern "C" fn(*mut c_void) -> *mut Ei,
    ei_setup_backend_fd: unsafe extern "C" fn(*mut Ei, RawFd) -> i32,
    ei_get_fd: unsafe extern "C" fn(*mut Ei) -> RawFd,
    ei_dispatch: unsafe extern "C" fn(*mut Ei),
    ei_get_event: unsafe extern "C" fn(*mut Ei) -> *mut EiEvent,
    ei_event_get_type: unsafe extern "C" fn(*mut EiEvent) -> i32,
    ei_event_get_device: unsafe extern "C" fn(*mut EiEvent) -> *mut EiDevice,
    ei_event_get_seat: unsafe extern "C" fn(*mut EiEvent) -> *mut EiSeat,
    ei_event_unref: Unref<EiEvent>,
    ei_unref: Unref<Ei>,
    ei_seat_bind_capabilities: unsafe extern "C" fn(*mut EiSeat, ...),
    ei_seat_has_capability: unsafe extern "C" fn(*mut EiSeat, i32) -> bool,
    ei_device_has_capability: unsafe extern "C" fn(*mut EiDevice, i32) -> bool,
    ei_device_ref: unsafe extern "C" fn(*mut EiDevice) -> *mut EiDevice,
    ei_device_unref: Unref<EiDevice>,
    ei_device_keyboard_get_keymap: unsafe extern "C" fn(*mut EiDevice) -> *mut EiKeymap,
    ei_keymap_get_fd: unsafe extern "C" fn(*mut EiKeymap) -> RawFd,
    ei_keymap_get_size: unsafe extern "C" fn(*mut EiKeymap) -> usize,
    ei_keymap_get_type: unsafe extern "C" fn(*mut EiKeymap) -> i32,
    ei_device_start_emulating: unsafe extern "C" fn(*mut EiDevice, u32),
    ei_device_keyboard_key: unsafe extern "C" fn(*mut EiDevice, u32, bool),
    ei_device_frame: unsafe extern "C" fn(*mut EiDevice, u64),
    ei_device_stop_emulating: unsafe extern "C" fn(*mut EiDevice),
    ei_now: unsafe extern "C" fn(*mut Ei) -> u64,
    ei_event_keyboard_get_xkb_mods_depressed: unsafe extern "C" fn(*mut EiEvent) -> u32,
    ei_event_keyboard_get_xkb_mods_latched: unsafe extern "C" fn(*mut EiEvent) -> u32,
    ei_event_keyboard_get_xkb_mods_locked: unsafe extern "C" fn(*mut EiEvent) -> u32,
    ei_event_keyboard_get_xkb_group: unsafe extern "C" fn(*mut EiEvent) -> u32,
}

#[derive(Clone, Copy)]
struct PingApi {
    create: unsafe extern "C" fn(*mut Ei) -> *mut EiPing,
    send: unsafe extern "C" fn(*mut EiPing),
    unref: Unref<EiPing>,
    answered: unsafe extern "C" fn(*mut EiEvent) -> *mut EiPing,
}

impl PingApi {
    unsafe fn load(library: &Library) -> Option<Self> {
        unsafe {
            Some(Self {
                create: *library
                    .get::<unsafe extern "C" fn(*mut Ei) -> *mut EiPing>(b"ei_new_ping\0")
                    .ok()?,
                send: *library
                    .get::<unsafe extern "C" fn(*mut EiPing)>(b"ei_ping\0")
                    .ok()?,
                unref: *library.get::<Unref<EiPing>>(b"ei_ping_unref\0").ok()?,
                answered: *library
                    .get::<unsafe extern "C" fn(*mut EiEvent) -> *mut EiPing>(
                        b"ei_event_pong_get_ping\0",
                    )
                    .ok()?,
            })
        }
    }
}

pub(super) struct Sender {
    keyboard: Option<Keyboard>,
    text: Option<Device>,
    context: Ref<Ei>,
    api: Api,
    keys: PasteKeys,
    sequence: u32,
    readiness: AsyncFd<OwnedFd>,
}

impl Sender {
    pub(super) fn connect(api: Api, fd: OwnedFd, keys: PasteKeys) -> anyhow::Result<Self> {
        let context = Ref {
            raw: NonNull::new(unsafe { (api.ei_new_sender)(ptr::null_mut()) })
                .context("Cannot create the Wayland input sender")?,
            unref: api.ei_unref,
        };

        let status = unsafe { (api.ei_setup_backend_fd)(context.as_ptr(), fd.into_raw_fd()) };
        ensure!(status == 0, "Cannot connect the Wayland input sender");

        let readiness = unsafe { register_readiness((api.ei_get_fd)(context.as_ptr())) }?;
        Ok(Self {
            keyboard: None,
            text: None,
            context,
            api,
            keys,
            sequence: 0,
            readiness,
        })
    }

    pub(super) const fn readiness(&self) -> &AsyncFd<OwnedFd> {
        &self.readiness
    }

    pub(super) fn can_paste(&self) -> bool {
        self.paste_target().is_ok()
    }

    pub(super) fn ready_paste(&mut self) -> Result<ReadyPaste<'_>, PasteBlocked> {
        let (keyboard, chord) = self.paste_target()?;
        Ok(ReadyPaste {
            sender: self,
            keyboard,
            chord,
        })
    }

    fn paste_target(&self) -> Result<(NonNull<EiDevice>, PasteChord<u32>), PasteBlocked> {
        let keyboard = self.keyboard.as_ref().ok_or(PasteBlocked::NoKeyboard)?;
        Ok((keyboard.device.handle.raw, keyboard.readiness()?))
    }

    pub(super) fn ready_text<'t>(
        &mut self,
        text: &'t str,
    ) -> Result<ReadyText<'_, 't>, TextBlocked> {
        let (Some(input), Some(device)) = (self.api.text_utf8, &self.text) else {
            return Err(TextBlocked::Unsupported);
        };
        if !device.resumed {
            return Err(TextBlocked::Paused);
        }
        if text.contains('\0') {
            return Err(TextBlocked::NullCharacter);
        }
        let device = device.handle.raw;
        Ok(ReadyText {
            sender: self,
            input,
            device,
            text,
        })
    }

    pub(super) fn dispatch(&mut self) -> anyhow::Result<()> {
        unsafe { (self.api.ei_dispatch)(self.context.as_ptr()) };
        while let Some(event) = self.next_event() {
            match event.kind {
                EVENT_DISCONNECT => bail!("Wayland input permission was disconnected"),
                EVENT_SEAT_ADDED => self.bind_seat(&event),
                EVENT_DEVICE_ADDED => self.add_device(&event),
                EVENT_DEVICE_REMOVED => self.remove_device(event.device),
                EVENT_DEVICE_PAUSED => self.set_resumed(event.device, false),
                EVENT_DEVICE_RESUMED => self.set_resumed(event.device, true),
                EVENT_KEYBOARD_MODIFIERS => self.update_modifiers(&event),
                EVENT_PONG => self.settle(&event),
                _ => {},
            }
        }
        Ok(())
    }

    fn next_event(&self) -> Option<Event> {
        unsafe {
            let handle = Ref {
                raw: NonNull::new((self.api.ei_get_event)(self.context.as_ptr()))?,
                unref: self.api.ei_event_unref,
            };
            Some(Event {
                kind: (self.api.ei_event_get_type)(handle.as_ptr()),
                device: NonNull::new((self.api.ei_event_get_device)(handle.as_ptr())),
                handle,
            })
        }
    }

    fn bind_seat(&self, event: &Event) {
        unsafe {
            let seat = (self.api.ei_event_get_seat)(event.handle.as_ptr());
            if seat.is_null() || !(self.api.ei_seat_has_capability)(seat, CAPABILITY_KEYBOARD) {
                return;
            }
            if self.api.text_utf8.is_some()
                && (self.api.ei_seat_has_capability)(seat, CAPABILITY_TEXT)
            {
                (self.api.ei_seat_bind_capabilities)(
                    seat,
                    CAPABILITY_KEYBOARD,
                    CAPABILITY_TEXT,
                    0_i32,
                );
            } else {
                (self.api.ei_seat_bind_capabilities)(seat, CAPABILITY_KEYBOARD, 0_i32);
            }
        }
    }

    fn add_device(&mut self, event: &Event) {
        let Some(device) = event.device else {
            return;
        };

        unsafe {
            if (self.api.ei_device_has_capability)(device.as_ptr(), CAPABILITY_KEYBOARD) {
                self.keyboard = Some(Keyboard {
                    device: Device::acquire(&self.api, device),
                    settling: None,
                    paste: device_keymap(&self.api, device.as_ptr())
                        .and_then(|keymap| PasteLayouts::new(&keymap, self.keys))
                        .map(|layouts| KeymapPaste {
                            layouts,
                            group: 0,
                            active_mask: 0,
                        }),
                });
            }
            if (self.api.ei_device_has_capability)(device.as_ptr(), CAPABILITY_TEXT) {
                self.text = Some(Device::acquire(&self.api, device));
            }
        }
    }

    fn remove_device(&mut self, device: Option<NonNull<EiDevice>>) {
        drop(self.keyboard.take_if(|keyboard| keyboard.device.is(device)));
        drop(self.text.take_if(|text| text.is(device)));
    }

    fn set_resumed(&mut self, device: Option<NonNull<EiDevice>>, resumed: bool) {
        if let Some(keyboard) = &mut self.keyboard
            && keyboard.device.is(device)
        {
            let settling = resumed.then(|| ping(&self.api, &self.context)).flatten();
            keyboard.set_resumed(resumed, settling);
        }
        if let Some(text) = &mut self.text
            && text.is(device)
        {
            text.resumed = resumed;
        }
    }

    fn settle(&mut self, event: &Event) {
        let (Some(calls), Some(keyboard)) = (self.api.ping, &mut self.keyboard) else {
            return;
        };
        keyboard.settle(NonNull::new(unsafe {
            (calls.answered)(event.handle.as_ptr())
        }));
    }

    fn update_modifiers(&mut self, event: &Event) {
        let Some(paste) = self
            .keyboard
            .as_mut()
            .filter(|keyboard| keyboard.device.is(event.device))
            .and_then(|keyboard| keyboard.paste.as_mut())
        else {
            return;
        };
        let raw = event.handle.as_ptr();

        let (group, active_mask) = unsafe {
            (
                (self.api.ei_event_keyboard_get_xkb_group)(raw),
                (self.api.ei_event_keyboard_get_xkb_mods_depressed)(raw)
                    | (self.api.ei_event_keyboard_get_xkb_mods_latched)(raw)
                    | (self.api.ei_event_keyboard_get_xkb_mods_locked)(raw),
            )
        };
        paste.group = group;
        paste.active_mask = active_mask;
    }

    fn next_sequence(&mut self) -> u32 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }
}

fn ping(api: &Api, context: &Ref<Ei>) -> Option<Ref<EiPing>> {
    let calls = api.ping?;
    let ping = Ref {
        raw: NonNull::new(unsafe { (calls.create)(context.as_ptr()) })?,
        unref: calls.unref,
    };
    unsafe { (calls.send)(ping.as_ptr()) };
    Some(ping)
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.keyboard = None;
        self.text = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PasteBlocked {
    NoKeyboard,
    NoKeyboardState,
    ShortcutKeys,
    NoPasteKeys,
}

fn paste_readiness(
    resumed: bool,
    paste: Option<&KeymapPaste>,
) -> Result<PasteChord<u32>, PasteBlocked> {
    if !resumed {
        return Err(PasteBlocked::NoKeyboard);
    }
    let paste = paste.ok_or(PasteBlocked::NoKeyboardState)?;
    if paste.active_mask & paste.layouts.blockers != 0 {
        return Err(PasteBlocked::ShortcutKeys);
    }
    paste
        .layouts
        .chord(paste.group)
        .ok_or(PasteBlocked::NoPasteKeys)
}

#[must_use]
pub(super) struct ReadyPaste<'a> {
    sender: &'a mut Sender,
    keyboard: NonNull<EiDevice>,
    chord: PasteChord<u32>,
}

impl ReadyPaste<'_> {
    pub(super) fn send(self) {
        let Self {
            sender,
            keyboard,
            chord,
        } = self;
        let sequence = sender.next_sequence();
        let api = &sender.api;
        let context = sender.context.as_ptr();
        let keyboard = keyboard.as_ptr();

        unsafe {
            (api.ei_device_start_emulating)(keyboard, sequence);
            for key in chord.presses() {
                (api.ei_device_keyboard_key)(keyboard, key, true);
            }
            (api.ei_device_frame)(keyboard, (api.ei_now)(context));
            for key in chord.presses().rev() {
                (api.ei_device_keyboard_key)(keyboard, key, false);
            }
            (api.ei_device_frame)(keyboard, (api.ei_now)(context));
            (api.ei_device_stop_emulating)(keyboard);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum TextBlocked {
    Unsupported,
    Paused,
    NullCharacter,
}

#[must_use]
pub(super) struct ReadyText<'a, 't> {
    sender: &'a mut Sender,
    input: TextInput,
    device: NonNull<EiDevice>,
    text: &'t str,
}

impl ReadyText<'_, '_> {
    pub(super) fn send(self) {
        let Self {
            sender,
            input,
            device,
            text,
        } = self;
        let sequence = sender.next_sequence();
        let api = &sender.api;
        let context = sender.context.as_ptr();
        let device = device.as_ptr();

        unsafe {
            (api.ei_device_start_emulating)(device, sequence);
            for chunk in utf8_chunks(text) {
                input(device, chunk.as_ptr(), chunk.len());
                (api.ei_device_frame)(device, (api.ei_now)(context));
            }
            (api.ei_device_stop_emulating)(device);
        }
    }
}

struct Keyboard {
    device: Device,
    settling: Option<Ref<EiPing>>,
    paste: Option<KeymapPaste>,
}

impl Keyboard {
    fn readiness(&self) -> Result<PasteChord<u32>, PasteBlocked> {
        paste_readiness(
            self.device.resumed && self.settling.is_none(),
            self.paste.as_ref(),
        )
    }

    fn set_resumed(&mut self, resumed: bool, settling: Option<Ref<EiPing>>) {
        self.device.resumed = resumed;
        self.settling = settling;
        if !resumed && let Some(paste) = &mut self.paste {
            paste.active_mask = 0;
        }
    }

    fn settle(&mut self, answered: Option<NonNull<EiPing>>) {
        drop(
            self.settling
                .take_if(|settling| Some(settling.raw) == answered),
        );
    }
}

struct KeymapPaste {
    layouts: PasteLayouts,
    group: u32,
    active_mask: u32,
}

struct Ref<T> {
    raw: NonNull<T>,
    unref: Unref<T>,
}

impl<T> Ref<T> {
    const fn as_ptr(&self) -> *mut T {
        self.raw.as_ptr()
    }
}

impl<T> Drop for Ref<T> {
    fn drop(&mut self) {
        unsafe { (self.unref)(self.raw.as_ptr()) };
    }
}

struct Device {
    handle: Ref<EiDevice>,
    resumed: bool,
}

impl Device {
    unsafe fn acquire(api: &Api, device: NonNull<EiDevice>) -> Self {
        unsafe { (api.ei_device_ref)(device.as_ptr()) };
        Self {
            handle: Ref {
                raw: device,
                unref: api.ei_device_unref,
            },
            resumed: false,
        }
    }

    fn is(&self, device: Option<NonNull<EiDevice>>) -> bool {
        device == Some(self.handle.raw)
    }
}

struct Event {
    handle: Ref<EiEvent>,
    kind: i32,
    device: Option<NonNull<EiDevice>>,
}

struct PasteLayouts {
    blockers: u32,
    chords: Vec<Option<PasteChord<u32>>>,
}

impl PasteLayouts {
    fn new(keymap: &xkb::Keymap, keys: PasteKeys) -> Option<Self> {
        let blockers = ["Shift", "Control", "Mod1", "Mod2", "Mod3", "Mod4", "Mod5"]
            .into_iter()
            .filter_map(|name| 1_u32.checked_shl(keymap.mod_get_index(name)))
            .fold(0, |mask, bit| mask | bit)
            & !num_lock_modifiers(keymap);
        (blockers != 0).then(|| Self {
            blockers,
            chords: (0..keymap.num_layouts())
                .map(|group| {
                    PasteChord::resolve(keys, |symbol| {
                        layout_key(keymap, group, symbol)?
                            .raw()
                            .checked_sub(XKB_KEYCODE_OFFSET)
                    })
                })
                .collect(),
        })
    }

    fn chord(&self, group: u32) -> Option<PasteChord<u32>> {
        *self.chords.get(usize::try_from(group).ok()?)?
    }
}

fn layout_key(keymap: &xkb::Keymap, group: u32, symbol: Keysym) -> Option<xkb::Keycode> {
    (keymap.min_keycode().raw()..=keymap.max_keycode().raw())
        .map(xkb::Keycode::new)
        .find(|&key| {
            group
                .checked_rem(keymap.num_layouts_for_key(key))
                .is_some_and(|layout| {
                    keymap
                        .key_get_syms_by_level(key, layout, 0)
                        .contains(&symbol)
                })
        })
}

fn num_lock_modifiers(keymap: &xkb::Keymap) -> u32 {
    let Some(key) = layout_key(keymap, 0, Keysym::Num_Lock) else {
        return 0;
    };
    let mut state = xkb::State::new(keymap);
    state.update_key(key, xkb::KeyDirection::Down);
    state.update_key(key, xkb::KeyDirection::Up);
    state.serialize_mods(xkb::STATE_MODS_LOCKED)
}

unsafe fn register_readiness(fd: RawFd) -> anyhow::Result<AsyncFd<OwnedFd>> {
    ensure!(fd >= 0, "Desktop connection has no valid file descriptor");

    super::watch_readable(unsafe { BorrowedFd::borrow_raw(fd) })
}

unsafe fn device_keymap(api: &Api, device: *mut EiDevice) -> Option<xkb::Keymap> {
    let (fd, size) = unsafe {
        let keymap = (api.ei_device_keyboard_get_keymap)(device);
        if keymap.is_null() || (api.ei_keymap_get_type)(keymap) != KEYMAP_XKB {
            return None;
        }
        (
            (api.ei_keymap_get_fd)(keymap),
            (api.ei_keymap_get_size)(keymap),
        )
    };
    if fd < 0 || !(1..=MAX_KEYMAP_SIZE).contains(&size) {
        return None;
    }

    let descriptor = unsafe { BorrowedFd::borrow_raw(fd) };
    let file = File::from(descriptor.try_clone_to_owned().ok()?);
    let mut bytes = vec![0; size];
    file.read_exact_at(&mut bytes, 0).ok()?;
    let mut source = String::from_utf8(bytes).ok()?;
    source.truncate(source.trim_end_matches('\0').len());
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_string(
        &context,
        source,
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
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
            let layouts = PasteLayouts::new(&keymap, PasteKeys::CtrlShiftV)
                .context("Fixture paste modifiers")?;
            assert_eq!(
                layouts.chord(group),
                Some(PasteChord {
                    control: 29,
                    shift: Some(42),
                    v: expected,
                })
            );
            assert!(layouts.chord(keymap.num_layouts()).is_none());
            assert_eq!(
                layouts.blockers & (1 << keymap.mod_get_index("Mod2")),
                0,
                "Num Lock blocks paste in {layout}"
            );
        }
        Ok(())
    }

    #[test]
    fn plain_paste_needs_no_shift_and_num_lock_never_blocks_wherever_it_maps() -> anyhow::Result<()>
    {
        let mask = |keymap: &xkb::Keymap, name: &str| 1_u32 << keymap.mod_get_index(name);
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_string(
            &context,
            r#"xkb_keymap {
                xkb_keycodes { include "evdev" };
                xkb_types { include "complete" };
                xkb_compat { include "complete" };
                xkb_symbols {
                    key <LCTL> { [ Control_L ] };
                    key <AB04> { [ v, V ] };
                    key <CAPS> { [ Caps_Lock ] };
                    key <NMLK> { [ Num_Lock ] };
                    modifier_map Control { <LCTL> };
                    modifier_map Lock { <CAPS> };
                    modifier_map Mod3 { <NMLK> };
                };
            };"#
            .to_owned(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .context("Fixture keymap")?;
        assert_eq!(
            PasteLayouts::new(&keymap, PasteKeys::CtrlV)
                .context("Fixture paste modifiers")?
                .chord(0),
            Some(PasteChord {
                control: 29,
                shift: None,
                v: 47,
            }),
            "A layout without Shift refused Ctrl+V"
        );
        let terminal =
            PasteLayouts::new(&keymap, PasteKeys::CtrlShiftV).context("Fixture paste modifiers")?;
        assert!(terminal.chord(0).is_none());
        assert_eq!(
            terminal.blockers & mask(&keymap, "Mod3"),
            0,
            "Num Lock blocks paste"
        );
        assert_eq!(
            terminal.blockers & mask(&keymap, "Lock"),
            0,
            "Caps Lock blocks paste"
        );
        assert_ne!(terminal.blockers & mask(&keymap, "Mod2"), 0);
        Ok(())
    }

    const CHORD: PasteChord<u32> = PasteChord {
        control: 29,
        shift: None,
        v: 47,
    };

    fn held_keymap() -> KeymapPaste {
        KeymapPaste {
            layouts: PasteLayouts {
                blockers: 1,
                chords: vec![Some(CHORD), None],
            },
            group: 1,
            active_mask: 1,
        }
    }

    #[test]
    fn paste_readiness_names_the_root_blocker_first() {
        let mut paste = held_keymap();
        assert_eq!(
            paste_readiness(false, Some(&paste)),
            Err(PasteBlocked::NoKeyboard),
            "A paused keyboard was reported as held keys or a missing paste key"
        );
        assert_eq!(
            paste_readiness(true, None),
            Err(PasteBlocked::NoKeyboardState),
            "An unreadable keymap read as held shortcut keys"
        );
        assert_eq!(
            paste_readiness(true, Some(&paste)),
            Err(PasteBlocked::ShortcutKeys),
            "Held keys were reported as a missing paste key"
        );
        paste.active_mask = 0;
        assert_eq!(
            paste_readiness(true, Some(&paste)),
            Err(PasteBlocked::NoPasteKeys)
        );
        paste.group = 0;
        assert_eq!(paste_readiness(true, Some(&paste)), Ok(CHORD));
    }

    #[test]
    fn a_resumed_keyboard_waits_for_its_ping_and_pausing_lifts_the_modifiers() {
        unsafe extern "C" fn keep<T>(raw: *mut T) -> *mut T {
            raw
        }
        let mut other = 0_u8;
        let (first, second) = (NonNull::dangling(), NonNull::from(&mut other).cast());
        let ping = |raw| Some(Ref { raw, unref: keep });
        let mut keyboard = Keyboard {
            device: Device {
                handle: Ref {
                    raw: NonNull::dangling(),
                    unref: keep,
                },
                resumed: false,
            },
            settling: None,
            paste: Some(KeymapPaste {
                group: 0,
                ..held_keymap()
            }),
        };
        keyboard.set_resumed(true, ping(first));
        assert_eq!(
            keyboard.readiness(),
            Err(PasteBlocked::NoKeyboard),
            "A resumed keyboard pasted before the compositor answered its ping"
        );
        keyboard.settle(Some(second));
        assert_eq!(
            keyboard.readiness(),
            Err(PasteBlocked::NoKeyboard),
            "Another ping's answer settled the keyboard"
        );
        keyboard.settle(Some(first));
        assert_eq!(
            keyboard.readiness(),
            Err(PasteBlocked::ShortcutKeys),
            "Resuming forgot the modifiers the compositor reported"
        );
        keyboard.set_resumed(false, None);
        assert_eq!(keyboard.readiness(), Err(PasteBlocked::NoKeyboard));
        keyboard.set_resumed(true, ping(second));
        keyboard.settle(Some(first));
        assert_eq!(
            keyboard.readiness(),
            Err(PasteBlocked::NoKeyboard),
            "An earlier resume's answer settled a later one"
        );
        keyboard.settle(Some(second));
        assert_eq!(
            keyboard.readiness(),
            Ok(CHORD),
            "Modifiers held when the keyboard paused still blocked paste after it resumed"
        );
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
        assert!(unsafe { register_readiness(-1) }.is_err());
    }
}
