mod clipboard;
#[cfg(test)]
mod native_tests;
mod paste;

use std::{convert::Infallible, os::fd::AsFd as _};

use anyhow::{Context as _, anyhow, bail, ensure};
use async_channel::Receiver;
use x11rb::{
    connection::Connection,
    errors::ReplyError,
    protocol::{
        ErrorKind, Event,
        xinput::{self, ConnectionExt as _},
        xproto::{self, Atom, ConnectionExt as _, Window},
    },
    xcb_ffi::XCBConnection,
};
use xkbcommon::xkb::{KEYSYM_CASE_INSENSITIVE, Keysym, keysym_from_name};

pub(super) use self::clipboard::ClipboardWorker;
use self::paste::InsertionWorker;
use super::{Actionable, HostSession, ShortcutLabels, stop_requested, watch_readable};
use crate::{DesktopOptions, Input, InputSender, LinuxPaste, insertion::Insertion};

const MAX_FOCUS_ANCESTRY: usize = 32;

const CORE_MODIFIERS: u16 = 0xFF;

#[derive(PartialEq, Eq)]
struct Keyboard {
    first_keycode: u8,
    symbols_per_key: usize,
    symbols: Vec<u32>,
    modifier_keys: Vec<u8>,
    keys_per_modifier: usize,
}

impl Keyboard {
    fn load(connection: &XCBConnection) -> anyhow::Result<Self> {
        let setup = connection.setup();
        let map = connection
            .get_keyboard_mapping(
                setup.min_keycode,
                setup
                    .max_keycode
                    .checked_sub(setup.min_keycode)
                    .and_then(|range| range.checked_add(1))
                    .context("Invalid X11 keyboard range")?,
            )?
            .reply()?;
        let modifiers = connection.get_modifier_mapping()?.reply()?;
        Ok(Self {
            first_keycode: setup.min_keycode,
            symbols_per_key: map.keysyms_per_keycode.into(),
            symbols: map.keysyms,
            keys_per_modifier: modifiers.keycodes_per_modifier().into(),
            modifier_keys: modifiers.keycodes,
        })
    }

    fn keys(&self) -> impl Iterator<Item = (u8, &[u32])> {
        let rows = (self.symbols_per_key != 0).then(|| self.symbols.chunks(self.symbols_per_key));
        (self.first_keycode..=u8::MAX).zip(rows.into_iter().flatten())
    }

    fn modifiers(&self) -> impl Iterator<Item = (u16, &[u8])> {
        let groups = (self.keys_per_modifier != 0)
            .then(|| self.modifier_keys.chunks(self.keys_per_modifier));
        (0..u16::BITS)
            .map(|bit| 1 << bit)
            .zip(groups.into_iter().flatten())
    }

    fn keycode(&self, symbol: Keysym) -> anyhow::Result<u8> {
        ensure!(symbol != Keysym::NoSymbol, "Unknown shortcut key");
        self.keys()
            .find_map(|(code, symbols)| symbols.contains(&symbol.raw()).then_some(code))
            .context("Shortcut key is unavailable in this keyboard layout")
    }

    fn unshifted_keycode(&self, symbol: Keysym) -> Option<u8> {
        self.keys()
            .find_map(|(code, symbols)| (symbols.first() == Some(&symbol.raw())).then_some(code))
    }

    fn modifier_mask(&self, modifier: Keysym) -> anyhow::Result<u16> {
        let code = self.keycode(modifier)?;
        ensure!(
            self.keys_per_modifier != 0,
            "Keyboard has no modifier mapping"
        );
        self.modifiers()
            .find_map(|(mask, codes)| codes.contains(&code).then_some(mask))
            .context("Shortcut modifier is not mapped")
    }

    fn lock_modifiers(&self) -> u16 {
        self.modifier_mask(Keysym::Caps_Lock).unwrap_or(0)
            | self.modifier_mask(Keysym::Num_Lock).unwrap_or(0)
    }

    fn held_modifiers(&self, keys: &[u8; 32]) -> u16 {
        self.modifiers()
            .filter(|(_, codes)| {
                codes
                    .iter()
                    .any(|&code| code != 0 && is_key_down(keys, code))
            })
            .fold(0, |held, (mask, _)| held | mask)
    }

    fn binding(&self, expression: &str) -> anyhow::Result<KeyBinding> {
        let (modifier_symbols, key) = parse_binding(expression)?;
        let mut modifiers = 0;
        for modifier in modifier_symbols {
            modifiers |= self.modifier_mask(modifier)?;
        }
        Ok(KeyBinding {
            key: self.keycode(keysym_from_name(key, KEYSYM_CASE_INSENSITIVE))?,
            modifiers,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct KeyBinding {
    key: u8,
    modifiers: u16,
}

#[derive(Clone, Copy)]
struct Shortcuts {
    dictate: KeyBinding,
    cancel: KeyBinding,
}

impl Shortcuts {
    fn bind(keyboard: &Keyboard, options: &DesktopOptions) -> anyhow::Result<Self> {
        let dictate = keyboard.binding(&options.shortcut)?;
        let cancel = keyboard.binding(&options.cancel)?;
        ensure!(
            dictate != cancel,
            "Dictate and cancel shortcuts must differ"
        );
        Ok(Self { dictate, cancel })
    }

    fn grab(self, connection: &XCBConnection, root: Window, lock: u16) -> anyhow::Result<()> {
        let caps = u16::from(xproto::ModMask::LOCK);
        let lock_states = [0, lock & caps, lock & !caps, lock];
        for (binding, taken) in [
            (
                self.dictate,
                "Another app already uses the dictation shortcut. Free it there or choose Shortcuts: Desktop bindings in Settings, then resume dictation.",
            ),
            (
                self.cancel,
                "Another app already uses the cancel shortcut. Free it there or choose Shortcuts: Desktop bindings in Settings, then resume dictation.",
            ),
        ] {
            for locks in lock_states {
                connection
                    .grab_key(
                        false,
                        root,
                        xproto::ModMask::from(binding.modifiers | locks),
                        binding.key,
                        xproto::GrabMode::ASYNC,
                        xproto::GrabMode::ASYNC,
                    )?
                    .check()
                    .map_err(|error| match error {
                        ReplyError::X11Error(error) if error.error_kind == ErrorKind::Access => {
                            anyhow!(Actionable(taken))
                        },
                        error => error.into(),
                    })?;
            }
        }
        Ok(())
    }
}

struct ShortcutTracker<'a> {
    connection: &'a XCBConnection,
    keyboard: &'a Keyboard,
    shortcuts: Option<Shortcuts>,
    lock: u16,
    depends_on_mapping: bool,
    input: &'a InputSender,
    held: Option<KeyBinding>,
}

impl ShortcutTracker<'_> {
    fn observe(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::KeyPress(key) => self.press(&key),
            Event::XinputRawKeyRelease(_) | Event::KeyRelease(_) => self.release(),
            Event::MappingNotify(_) if self.depends_on_mapping => self.check_mapping(),
            _ => Ok(()),
        }
    }

    fn check_mapping(&self) -> anyhow::Result<()> {
        ensure!(
            Keyboard::load(self.connection)? == *self.keyboard,
            Actionable("Keyboard mapping changed. Resume dictation to bind the new layout.")
        );
        Ok(())
    }

    fn press(&mut self, key: &xproto::KeyPressEvent) -> anyhow::Result<()> {
        let pressed = KeyBinding {
            key: key.detail,
            modifiers: u16::from(key.state) & CORE_MODIFIERS & !self.lock,
        };
        if let Some(shortcuts) = self.shortcuts {
            if shortcuts.cancel == pressed {
                self.input.deliver(Input::Cancel);
            } else if shortcuts.dictate == pressed && self.held.is_none() {
                self.held = Some(shortcuts.dictate);
                self.input.deliver(Input::Press);
            }
        }

        self.connection.ungrab_keyboard(key.time)?;
        self.connection.flush()?;
        Ok(())
    }

    fn release(&mut self) -> anyhow::Result<()> {
        let Some(dictate) = self.held else {
            return Ok(());
        };
        let keys = self.connection.query_keymap()?.reply()?.keys;
        if !is_key_down(&keys, dictate.key)
            || self.keyboard.held_modifiers(&keys) & dictate.modifiers != dictate.modifiers
        {
            self.held = None;
            self.input.deliver(Input::Release);
        }
        Ok(())
    }
}

pub(super) async fn run(
    input: &InputSender,
    options: &DesktopOptions,
    requests: Receiver<Insertion>,
    stopping: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    let (connection, screen) = XCBConnection::connect(None)?;
    let root = connection
        .setup()
        .roots
        .get(screen)
        .context("X11 display has no screen")?
        .root;
    let keyboard = Keyboard::load(&connection)?;
    let shortcuts = if options.external_shortcut {
        None
    } else {
        Some(Shortcuts::bind(&keyboard, options)?)
    };
    let lock = keyboard.lock_modifiers();

    let worker = InsertionWorker::start(input.clone(), options.clone(), requests)?;
    tokio::select! {
        biased;
        () = stop_requested(stopping, input) => return Ok(()),
        ready = worker.ready() => ready?,
    }
    if let Some(shortcuts) = shortcuts {
        shortcuts.grab(&connection, root, lock)?;
        observe_releases(&connection, root)?;
    }
    let readiness = watch_readable(connection.as_fd())?;
    input.deliver(ShortcutLabels::new(options).into_desktop_ready());
    let mut tracker = ShortcutTracker {
        connection: &connection,
        keyboard: &keyboard,
        shortcuts,
        lock,
        depends_on_mapping: depends_on_mapping(options),
        input,
        held: None,
    };
    loop {
        while let Some(event) = connection.poll_for_event()? {
            tracker.observe(event)?;
        }
        tokio::select! {
            biased;
            () = stop_requested(stopping, input) => break,
            readable = readiness.readable() => readable?.clear_ready(),
        }
    }
    Ok(())
}

fn depends_on_mapping(options: &DesktopOptions) -> bool {
    !(options.external_shortcut && options.paste == LinuxPaste::Manual)
}

fn observe_releases(connection: &XCBConnection, root: Window) -> anyhow::Result<()> {
    connection.xinput_xi_query_version(2, 2)?.reply()?;
    connection
        .xinput_xi_select_events(
            root,
            &[xinput::EventMask {
                deviceid: xinput::Device::ALL_MASTER.into(),
                mask: vec![xinput::XIEventMask::RAW_KEY_RELEASE],
            }],
        )?
        .check()?;
    Ok(())
}

fn is_key_down(keys: &[u8; 32], code: u8) -> bool {
    keys.get(usize::from(code / 8))
        .is_some_and(|bits| bits & (1 << (code % 8)) != 0)
}

fn parse_binding(expression: &str) -> anyhow::Result<(Vec<Keysym>, &str)> {
    const BINDING_MODIFIER_KEYS: [&str; 8] = [
        "Control_L",
        "Control_R",
        "Super_L",
        "Super_R",
        "Alt_L",
        "Alt_R",
        "Shift_L",
        "Shift_R",
    ];
    let mut parts = expression.split('+').map(str::trim);
    let key = parts
        .next_back()
        .filter(|key| !key.is_empty())
        .context("Shortcut needs an ordinary key")?;
    let modifiers = parts
        .map(|part| match part {
            "CTRL" => Ok(Keysym::Control_L),
            "LOGO" => Ok(Keysym::Super_L),
            "ALT" => Ok(Keysym::Alt_L),
            "SHIFT" => Ok(Keysym::Shift_L),
            _ => bail!("Use CTRL, LOGO, ALT or SHIFT before the shortcut key"),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(
        (1..=3).contains(&modifiers.len()),
        "Shortcut needs one to three modifiers"
    );
    ensure!(
        !BINDING_MODIFIER_KEYS
            .iter()
            .any(|name| name.eq_ignore_ascii_case(key)),
        "Shortcut needs an ordinary key"
    );
    Ok((modifiers, key))
}

fn has_external_target(connection: &XCBConnection, host: HostSession) -> anyhow::Result<bool> {
    let pid = intern(connection, b"_NET_WM_PID")?;
    let focused = connection.get_input_focus()?.reply()?.focus;
    focus_is_external(host, focused, std::process::id(), |window| {
        let owner = connection
            .get_property(false, window, pid, xproto::AtomEnum::CARDINAL, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut values| values.next());
        let parent = connection.query_tree(window)?.reply()?.parent;
        Ok((owner, parent))
    })
}

fn focus_is_external(
    host: HostSession,
    mut window: Window,
    own_pid: u32,
    mut inspect: impl FnMut(Window) -> anyhow::Result<(Option<u32>, Window)>,
) -> anyhow::Result<bool> {
    if window == x11rb::NONE {
        return Ok(host == HostSession::Wayland);
    }
    if window == u32::from(xproto::InputFocus::POINTER_ROOT) {
        return Ok(false);
    }
    for _ in 0..MAX_FOCUS_ANCESTRY {
        let (pid, next) = inspect(window)?;
        if let Some(pid) = pid {
            return Ok(pid != own_pid);
        }
        if next == x11rb::NONE {
            return Ok(true);
        }
        if next == window {
            return Ok(false);
        }
        window = next;
    }
    Ok(false)
}

fn intern(connection: &XCBConnection, name: &[u8]) -> anyhow::Result<Atom> {
    Ok(connection.intern_atom(false, name)?.reply()?.atom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unqueried(_: Window) -> anyhow::Result<(Option<u32>, Window)> {
        bail!("Queried a focus that names no window")
    }

    #[test]
    fn focus_guard_rejects_own_child_windows_and_unknown_focus() -> anyhow::Result<()> {
        let own_child_field = |window| {
            Ok(if window == 20 {
                (None, 10)
            } else {
                (Some(7), 0)
            })
        };
        let other_process = |_| Ok((Some(8), 0));
        let cyclic_tree = |_| Ok((None, 20));
        let disconnected = |_| bail!("Disconnected");
        let pointer_root = u32::from(xproto::InputFocus::POINTER_ROOT);
        assert!(!focus_is_external(
            HostSession::X11,
            20,
            7,
            own_child_field
        )?);
        assert!(focus_is_external(HostSession::X11, 20, 7, other_process)?);
        assert!(!focus_is_external(HostSession::X11, 20, 7, cyclic_tree)?);
        assert!(focus_is_external(HostSession::X11, 20, 7, disconnected).is_err());
        for host in [HostSession::X11, HostSession::Wayland] {
            assert!(!focus_is_external(host, pointer_root, 7, unqueried)?);
        }
        assert!(!focus_is_external(
            HostSession::X11,
            x11rb::NONE,
            7,
            unqueried
        )?);
        assert!(
            focus_is_external(HostSession::Wayland, x11rb::NONE, 7, unqueried)?,
            "Wayland focus moved to a native editor"
        );
        Ok(())
    }

    #[test]
    fn shortcuts_require_explicit_modifiers_and_an_ordinary_key() -> anyhow::Result<()> {
        assert_eq!(
            parse_binding("CTRL+LOGO+space")?,
            (vec![Keysym::Control_L, Keysym::Super_L], "space")
        );
        for binding in [
            "space",
            "CTRL+",
            "CTRL+Super_L",
            "CTRL+super_l",
            "CTRL+UNKNOWN+space",
        ] {
            assert!(
                parse_binding(binding).is_err(),
                "{binding} parsed as a shortcut"
            );
        }
        Ok(())
    }

    #[test]
    fn only_grabs_or_automatic_paste_stop_on_a_keyboard_mapping_change() {
        for (external_shortcut, paste, depends) in [
            (false, LinuxPaste::Standard, true),
            (false, LinuxPaste::Manual, true),
            (true, LinuxPaste::Standard, true),
            (true, LinuxPaste::Terminal, true),
            (true, LinuxPaste::Manual, false),
        ] {
            let options = DesktopOptions {
                paste,
                external_shortcut,
                ..DesktopOptions::default()
            };
            assert_eq!(
                depends_on_mapping(&options),
                depends,
                "Desktop bindings {external_shortcut}, {paste:?} paste"
            );
        }
    }

    #[test]
    fn paste_keys_require_an_unshifted_symbol_in_the_first_layout() {
        let keyboard = Keyboard {
            first_keycode: 8,
            symbols_per_key: 4,
            symbols: vec![0x61, 0x41, 0x76, 0x56, 0x76, 0x56, 0x61, 0x41],
            modifier_keys: Vec::new(),
            keys_per_modifier: 0,
        };
        assert_eq!(keyboard.unshifted_keycode(Keysym::v), Some(9));
        assert_eq!(keyboard.unshifted_keycode(Keysym::V), None);
    }
}
