//! The dynamically loaded libei sender that types the paste shortcut and direct text on Wayland.
//! Modifiers stay unknown until libei reports them, never assumed released.

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
use libloading::os::unix::{Library, RTLD_LAZY, RTLD_LOCAL};
use tokio::io::unix::AsyncFd;
use xkbcommon::xkb::{self, Keysym};

use super::PasteChord;

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

type Unref<T> = unsafe extern "C" fn(*mut T) -> *mut T;
type TextInput = unsafe extern "C" fn(*mut EiDevice, *const u8, usize);

macro_rules! api {
    ($($name:ident: $kind:ty),+ $(,)?) => {
        /// libei's entry points; `text_utf8` is newer than libei 1.5, so it alone is optional.
        pub(super) struct Api { $($name: $kind,)+ text_utf8: Option<TextInput> }
        impl Api {
            /// Loads libei, which keyboard input needs before the user is asked to allow it.
            ///
            /// # Errors
            /// Fails when libei or one of its required entry points is missing.
            pub(super) fn load() -> anyhow::Result<Self> {
                // SAFETY: each type is the symbol's C signature in libei's public ABI. RTLD_NODELETE
                // keeps libei mapped for the life of the process, so the copied function pointers
                // stay valid after the handle drops here.
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
                    Ok(Self { $($name,)+ text_utf8 })
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

pub(super) struct Sender {
    keyboard: Option<Keyboard>,
    text: Option<Device>,
    context: EiContext,
    api: Api,
    /// Whether paste presses Ctrl+Shift+V, so a layout needs Shift to paste.
    terminal: bool,
    sequence: u32,
    readiness: AsyncFd<OwnedFd>,
}

impl Sender {
    /// Connects to the EIS descriptor for keyboard input that pastes with Ctrl+Shift+V when
    /// `terminal`, or Ctrl+V otherwise.
    pub(super) fn connect(api: Api, fd: OwnedFd, terminal: bool) -> anyhow::Result<Self> {
        let context = EiContext::new_sender(&api)?;
        // SAFETY: libei takes ownership of the descriptor, including its teardown.
        let status = unsafe { (api.ei_setup_backend_fd)(context.as_ptr(), fd.into_raw_fd()) };
        ensure!(status == 0, "Could not connect Wayland input sender");
        // SAFETY: the live context keeps its descriptor open while it is duplicated.
        let readiness = unsafe { register_readiness((api.ei_get_fd)(context.as_ptr())) }?;
        Ok(Self {
            keyboard: None,
            text: None,
            context,
            api,
            terminal,
            sequence: 0,
            readiness,
        })
    }

    pub(super) const fn readiness(&self) -> &AsyncFd<OwnedFd> {
        &self.readiness
    }

    /// Treats modifiers as unknown until libei reports them again.
    pub(super) fn forget_modifiers(&mut self) {
        if let Some(keyboard) = &mut self.keyboard {
            keyboard.forget_modifiers();
        }
    }

    /// Whether automatic paste can run right now.
    pub(super) fn can_paste(&self) -> bool {
        self.paste_target().is_ok()
    }

    /// Prepares a paste through the resumed keyboard once libei has reported every paste-blocking
    /// modifier released.
    ///
    /// # Errors
    /// Returns what stops automatic paste.
    pub(super) fn ready_paste(&mut self) -> Result<ReadyPaste<'_>, PasteBlocked> {
        let (keyboard, chord) = self.paste_target()?;
        Ok(ReadyPaste {
            sender: self,
            keyboard,
            chord,
        })
    }

    fn paste_target(&self) -> Result<(*mut EiDevice, PasteChord<u32>), PasteBlocked> {
        let keyboard = self.keyboard.as_ref().ok_or(PasteBlocked::NoKeyboard)?;
        let chord = paste_readiness(
            keyboard.device.resumed,
            keyboard.paste.as_ref().map(|paste| paste.state),
        )?;
        Ok((keyboard.device.as_ptr(), chord))
    }

    /// Prepares `text` for the resumed text device.
    ///
    /// # Errors
    /// Returns what stops direct text input.
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
        let device = device.as_ptr();
        Ok(ReadyText {
            sender: self,
            input,
            device,
            text,
        })
    }

    /// Applies queued libei events to the sender's devices.
    ///
    /// # Errors
    /// Fails when libei reports a disconnect. That is libei's last event, so later calls succeed:
    /// the caller must treat the error as the end of desktop access.
    pub(super) fn dispatch(&mut self) -> anyhow::Result<()> {
        // SAFETY: the context is live and dispatched only on its owning thread.
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
            if self.api.text_utf8.is_some()
                && (self.api.ei_seat_has_capability)(seat, CAPABILITY_TEXT)
            {
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
                self.keyboard = Some(Keyboard {
                    device: Device::acquire(&self.api, device),
                    paste: device_keymap(&self.api, device.as_ptr())
                        .and_then(|keymap| PasteLayouts::new(&keymap, self.terminal))
                        .ok()
                        .map(|layouts| KeymapPaste {
                            layouts,
                            state: PasteState::Unknown,
                        }),
                });
            }
            if (self.api.ei_device_has_capability)(device.as_ptr(), CAPABILITY_TEXT) {
                self.text = Some(Device::acquire(&self.api, device));
            }
        }
    }

    /// Releases the sender's references to a removed `device`; the removal event still holds its own
    /// until it drops.
    fn remove_device(&mut self, device: Option<NonNull<EiDevice>>) {
        drop(self.keyboard.take_if(|keyboard| keyboard.device.is(device)));
        drop(self.text.take_if(|text| text.is(device)));
    }

    fn set_resumed(&mut self, device: Option<NonNull<EiDevice>>, resumed: bool) {
        if let Some(keyboard) = self
            .keyboard
            .as_mut()
            .filter(|keyboard| keyboard.device.is(device))
        {
            keyboard.device.resumed = resumed;
            keyboard.forget_modifiers();
        }
        if let Some(text) = self.text.as_mut().filter(|text| text.is(device)) {
            text.resumed = resumed;
        }
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
        let raw = event.raw.as_ptr();
        // SAFETY: the live event reports keyboard modifiers.
        let (group, active_mask) = unsafe {
            (
                (self.api.ei_event_keyboard_get_xkb_group)(raw),
                (self.api.ei_event_keyboard_get_xkb_mods_depressed)(raw)
                    | (self.api.ei_event_keyboard_get_xkb_mods_latched)(raw)
                    | (self.api.ei_event_keyboard_get_xkb_mods_locked)(raw),
            )
        };
        paste.state = if active_mask & paste.layouts.blockers == 0 {
            paste
                .layouts
                .chord(group)
                .map_or(PasteState::NoKeys, PasteState::Ready)
        } else {
            PasteState::Blocked
        };
    }

    /// Numbers the next emulation sequence.
    fn next_sequence(&mut self) -> u32 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }
}

impl Drop for Sender {
    /// Releases device references before the context, whatever the field order: destroying a
    /// device unregisters it from the context it reaches through its seat, and a seat holds no
    /// context reference of its own.
    fn drop(&mut self) {
        self.keyboard = None;
        self.text = None;
    }
}

/// Why automatic paste cannot run. Each blocker has its own remedy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PasteBlocked {
    /// libei has added no keyboard, or has paused it.
    NoKeyboard,
    /// The keyboard's keymap could not be read, so its modifiers can never be judged.
    NoKeyboardState,
    /// A shortcut modifier is held, or libei has not reported modifiers since they could have
    /// changed.
    ShortcutKeys,
    /// The active layout cannot type the paste shortcut.
    NoPasteKeys,
}

/// Checks the paste blockers in a fixed order, so the root one is reported.
fn paste_readiness(
    resumed: bool,
    state: Option<PasteState>,
) -> Result<PasteChord<u32>, PasteBlocked> {
    if !resumed {
        return Err(PasteBlocked::NoKeyboard);
    }
    match state.ok_or(PasteBlocked::NoKeyboardState)? {
        PasteState::Unknown | PasteState::Blocked => Err(PasteBlocked::ShortcutKeys),
        PasteState::NoKeys => Err(PasteBlocked::NoPasteKeys),
        PasteState::Ready(chord) => Ok(chord),
    }
}

/// A paste every readiness check allowed. It borrows the sender exclusively, so no libei event can
/// pause or remove the keyboard before [`send`](Self::send).
#[must_use]
pub(super) struct ReadyPaste<'a> {
    sender: &'a mut Sender,
    keyboard: *mut EiDevice,
    chord: PasteChord<u32>,
}

impl ReadyPaste<'_> {
    /// Presses and releases the paste shortcut.
    pub(super) fn send(self) {
        let Self {
            sender,
            keyboard,
            chord,
        } = self;
        let sequence = sender.next_sequence();
        let api = &sender.api;
        let context = sender.context.as_ptr();
        // SAFETY: keyboard is the sender's referenced keyboard-capable device, and the exclusive
        // borrow has kept it resumed since the readiness check. The complete sequence releases
        // every injected key before stopping.
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

/// Why direct text input cannot run.
#[derive(Clone, Copy)]
pub(super) enum TextBlocked {
    /// libei or the desktop offers no text device.
    Unsupported,
    /// libei has paused the text device.
    Paused,
    /// EI text cannot carry a null character.
    NullCharacter,
}

/// Text every readiness check allowed. It borrows the sender exclusively, so no libei event can
/// pause or remove the text device before [`send`](Self::send).
#[must_use]
pub(super) struct ReadyText<'a, 't> {
    sender: &'a mut Sender,
    input: TextInput,
    device: *mut EiDevice,
    text: &'t str,
}

impl ReadyText<'_, '_> {
    /// Types the text in frames within the EI text limit.
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
        // SAFETY: device is the sender's referenced text-capable device, and the exclusive borrow
        // has kept it resumed since the readiness check. Each chunk stays alive for its C call and
        // holds no null character, and each frame respects the EI UTF-8 frame limit.
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

/// The sender's keyboard device and what paste needs to know about it. Adding a keyboard replaces
/// the whole value, so nothing learned about an earlier device survives.
struct Keyboard {
    device: Device,
    /// `None` when the device's keymap could not be read, so its modifiers can never be judged.
    paste: Option<KeymapPaste>,
}

impl Keyboard {
    fn forget_modifiers(&mut self) {
        if let Some(paste) = &mut self.paste {
            paste.state = PasteState::Unknown;
        }
    }
}

/// The paste layouts of a readable keymap, with the modifier state libei last reported.
struct KeymapPaste {
    layouts: PasteLayouts,
    state: PasteState,
}

/// Whether libei last reported every paste-blocking modifier released, with the active layout's
/// paste chord.
#[derive(Clone, Copy)]
enum PasteState {
    /// Not reported since the device arrived, paused, or resumed, or since the shortcut was
    /// pressed.
    Unknown,
    /// A paste-blocking modifier is held.
    Blocked,
    /// Modifiers are released, but the active layout cannot type the paste shortcut.
    NoKeys,
    Ready(PasteChord<u32>),
}

/// The sender's owning reference to its libei context.
struct EiContext {
    raw: NonNull<Ei>,
    unref: Unref<Ei>,
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

    const fn as_ptr(&self) -> *mut Ei {
        self.raw.as_ptr()
    }
}

impl Drop for EiContext {
    fn drop(&mut self) {
        // SAFETY: this is the context's only owning reference, and no device reference outlives
        // it: only `Sender` acquires devices, and `Sender::drop` releases them before this field
        // drops. Dropping it closes the EIS descriptor.
        unsafe { (self.unref)(self.raw.as_ptr()) };
    }
}

/// A device reference the sender holds, released when dropped.
struct Device {
    raw: NonNull<EiDevice>,
    /// Whether libei has resumed the device, so it accepts emulated input.
    resumed: bool,
    unref: Unref<EiDevice>,
}

impl Device {
    /// References `device`, which stays paused until libei resumes it.
    ///
    /// # Safety
    /// `device` must be a live libei device.
    unsafe fn acquire(api: &Api, device: NonNull<EiDevice>) -> Self {
        // SAFETY: the caller keeps the device live while this reference is taken.
        unsafe { (api.ei_device_ref)(device.as_ptr()) };
        Self {
            raw: device,
            resumed: false,
            unref: api.ei_device_unref,
        }
    }

    fn is(&self, device: Option<NonNull<EiDevice>>) -> bool {
        device == Some(self.raw)
    }

    const fn as_ptr(&self) -> *mut EiDevice {
        self.raw.as_ptr()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: the guard holds this reference, and `Sender::drop` releases devices before their
        // context.
        unsafe { (self.unref)(self.raw.as_ptr()) };
    }
}

/// One queued libei event, released when dropped.
struct Event {
    raw: NonNull<EiEvent>,
    kind: i32,
    device: Option<NonNull<EiDevice>>,
    unref: Unref<EiEvent>,
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the guard holds the event's only reference.
        unsafe { (self.unref)(self.raw.as_ptr()) };
    }
}

/// What paste needs from a keyboard's keymap, read once when libei advertises it.
struct PasteLayouts {
    /// The modifier mask that changes what the paste shortcut means.
    blockers: u32,
    /// Each layout's paste chord as evdev codes, indexed by XKB group.
    chords: Vec<Option<PasteChord<u32>>>,
}

impl PasteLayouts {
    /// Reads `keymap` for a paste that presses Ctrl+Shift+V when `terminal`, or Ctrl+V otherwise.
    fn new(keymap: &xkb::Keymap, terminal: bool) -> anyhow::Result<Self> {
        // Locked modifiers count as active, yet neither lock changes Ctrl+V: Caps Lock is always
        // Lock, and Num Lock is whatever the keymap's Num Lock key locks. Blocking on either would
        // disable paste while it is on.
        let blockers = ["Shift", "Control", "Mod1", "Mod2", "Mod3", "Mod4", "Mod5"]
            .into_iter()
            .filter_map(|name| 1_u32.checked_shl(keymap.mod_get_index(name)))
            .fold(0, |mask, bit| mask | bit)
            & !num_lock_modifiers(keymap);
        ensure!(blockers != 0, "Missing paste modifier mapping");
        Ok(Self {
            blockers,
            chords: (0..keymap.num_layouts())
                .map(|group| {
                    PasteChord::resolve(terminal, |symbol| {
                        layout_key(keymap, group, symbol)?
                            .raw()
                            .checked_sub(XKB_KEYCODE_OFFSET)
                    })
                })
                .collect(),
        })
    }

    /// The paste chord of layout `group`; none for a group past the last layout, which never
    /// wraps to another layout's chord.
    fn chord(&self, group: u32) -> Option<PasteChord<u32>> {
        *self.chords.get(usize::try_from(group).ok()?)?
    }
}

/// The first key whose base level types `symbol` in layout `group`. A key with fewer layouts than
/// the keymap wraps the group, as XKB does.
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

/// The modifiers the keymap's Num Lock key locks, in the masks libei reports; none without that
/// key.
fn num_lock_modifiers(keymap: &xkb::Keymap) -> u32 {
    let Some(key) = layout_key(keymap, 0, Keysym::Num_Lock) else {
        return 0;
    };
    let mut state = xkb::State::new(keymap);
    state.update_key(key, xkb::KeyDirection::Down);
    state.update_key(key, xkb::KeyDirection::Up);
    state.serialize_mods(xkb::STATE_MODS_LOCKED)
}

/// Watches a duplicate of libei's descriptor for readability.
///
/// # Safety
/// A non-negative `fd` must stay open until this returns.
unsafe fn register_readiness(fd: RawFd) -> anyhow::Result<AsyncFd<OwnedFd>> {
    ensure!(fd >= 0, "Desktop connection has no valid file descriptor");
    // SAFETY: the caller keeps fd open while it is duplicated; only the duplicate is owned.
    super::watch_readable(unsafe { BorrowedFd::borrow_raw(fd) })
}

/// Compiles the XKB keymap libei advertises for `device`.
///
/// # Safety
/// `device` must be a live keyboard-capable libei device.
unsafe fn device_keymap(api: &Api, device: *mut EiDevice) -> anyhow::Result<xkb::Keymap> {
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
    xkb::Keymap::new_from_string(
        &context,
        source,
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .context("Invalid XKB keymap")
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
            let layouts = PasteLayouts::new(&keymap, true)?;
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
            PasteLayouts::new(&keymap, false)?.chord(0),
            Some(PasteChord {
                control: 29,
                shift: None,
                v: 47,
            }),
            "A layout without Shift refused Ctrl+V"
        );
        let terminal = PasteLayouts::new(&keymap, true)?;
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

    #[test]
    fn paste_readiness_names_the_root_blocker_first() {
        let chord = PasteChord {
            control: 29,
            shift: None,
            v: 47,
        };
        for state in [None, Some(PasteState::Ready(chord))] {
            assert_eq!(paste_readiness(false, state), Err(PasteBlocked::NoKeyboard));
        }
        assert_eq!(
            paste_readiness(true, None),
            Err(PasteBlocked::NoKeyboardState),
            "An unreadable keymap read as held shortcut keys"
        );
        for state in [PasteState::Unknown, PasteState::Blocked] {
            assert_eq!(
                paste_readiness(true, Some(state)),
                Err(PasteBlocked::ShortcutKeys)
            );
        }
        assert_eq!(
            paste_readiness(true, Some(PasteState::NoKeys)),
            Err(PasteBlocked::NoPasteKeys)
        );
        assert_eq!(
            paste_readiness(true, Some(PasteState::Ready(chord))),
            Ok(chord)
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
        // SAFETY: a negative descriptor is rejected before it is borrowed.
        assert!(unsafe { register_readiness(-1) }.is_err());
    }
}
