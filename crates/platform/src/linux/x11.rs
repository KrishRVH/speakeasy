use super::*;
use anyhow::{Context, ensure};
use std::{
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
    time::Instant,
};
use tokio::io::unix::AsyncFd;
use x11rb::{
    connection::Connection,
    protocol::{
        Event, shape, xinput, xkb,
        xproto::{self, ConnectionExt as _},
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

pub(super) fn readiness(fd: i32) -> anyhow::Result<AsyncFd<OwnedFd>> {
    ensure!(fd >= 0, "Desktop connection has no valid file descriptor");
    // SAFETY: callers keep their connection alive for the borrowed descriptor;
    // only the duplicate is owned and closed by the readiness registration.
    let duplicate = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
    Ok(AsyncFd::new(duplicate)?)
}
struct Keyboard {
    first: u8,
    columns: usize,
    symbols: Vec<u32>,
    modifiers: Vec<u8>,
    per_modifier: usize,
}
pub(super) struct Clipboard {
    connection: RustConnection,
    native: arboard::Clipboard,
    selection: u32,
}
impl Clipboard {
    pub fn new() -> anyhow::Result<Self> {
        let (connection, _) = x11rb::connect(None)?;
        let selection = connection.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
        Ok(Self {
            connection,
            native: arboard::Clipboard::new()?,
            selection,
        })
    }
    pub fn owner(&self) -> anyhow::Result<u32> {
        Ok(self
            .connection
            .get_selection_owner(self.selection)?
            .reply()?
            .owner)
    }
    pub fn owns(&self, owner: u32) -> anyhow::Result<bool> {
        Ok(owner != 0 && self.owner()? == owner)
    }
    pub fn set_text(&mut self, text: &str) -> anyhow::Result<Option<u32>> {
        self.native.set_text(text)?;
        let owner = self.owner()?;
        // Another application can take ownership as soon as set_text returns.
        // Verify both the payload and its stable owner before authorizing paste.
        Ok((owner != 0 && self.native.get_text()? == text && self.owns(owner)?).then_some(owner))
    }
}
impl Keyboard {
    fn load(connection: &RustConnection) -> anyhow::Result<Self> {
        let setup = connection.setup();
        let map = connection
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)?
            .reply()?;
        let modifiers = connection.get_modifier_mapping()?.reply()?;
        Ok(Self {
            first: setup.min_keycode,
            columns: map.keysyms_per_keycode.into(),
            symbols: map.keysyms,
            per_modifier: modifiers.keycodes_per_modifier().into(),
            modifiers: modifiers.keycodes,
        })
    }
    fn code(&self, name: &str) -> anyhow::Result<u8> {
        let symbol =
            xkbcommon::xkb::keysym_from_name(name, xkbcommon::xkb::KEYSYM_CASE_INSENSITIVE).raw();
        ensure!(symbol != 0 && self.columns != 0, "Unknown shortcut key");
        let position = self
            .symbols
            .chunks(self.columns)
            .position(|symbols| symbols.contains(&symbol))
            .context("Shortcut key is unavailable in this keyboard layout")?;
        Ok(self.first + u8::try_from(position)?)
    }
    fn unshifted_code(&self, symbol: u32) -> Option<u8> {
        if self.columns == 0 {
            return None;
        }
        let position = self
            .symbols
            .chunks(self.columns)
            .position(|symbols| symbols.first() == Some(&symbol))?;
        self.first.checked_add(u8::try_from(position).ok()?)
    }
    fn modifier(&self, name: &str) -> anyhow::Result<u16> {
        let code = self.code(name)?;
        ensure!(self.per_modifier != 0, "Keyboard has no modifier mapping");
        let group = self
            .modifiers
            .chunks(self.per_modifier)
            .position(|codes| codes.contains(&code))
            .context("Shortcut modifier is not mapped")?;
        Ok(1 << group)
    }
    fn held(&self, keys: &[u8; 32]) -> u16 {
        if self.per_modifier == 0 {
            return 0;
        }
        self.modifiers
            .chunks(self.per_modifier)
            .enumerate()
            .fold(0, |bits, (group, codes)| {
                if codes
                    .iter()
                    .any(|&code| code != 0 && keys[usize::from(code / 8)] & (1 << (code % 8)) != 0)
                {
                    bits | (1 << group)
                } else {
                    bits
                }
            })
    }
    fn binding(&self, expression: &str) -> anyhow::Result<(u8, u16)> {
        let (modifiers, key) = parse_binding(expression)?;
        let bits = modifiers.iter().try_fold(0, |bits, name| {
            Ok::<_, anyhow::Error>(bits | self.modifier(name)?)
        })?;
        Ok((self.code(key)?, bits))
    }
}
fn parse_binding(expression: &str) -> anyhow::Result<(Vec<&'static str>, &str)> {
    let mut parts: Vec<_> = expression.split('+').map(str::trim).collect();
    let key = parts
        .pop()
        .filter(|key| !key.is_empty())
        .context("Shortcut needs an ordinary key")?;
    let modifiers = parts
        .into_iter()
        .map(|part| match part {
            "CTRL" => Ok("Control_L"),
            "LOGO" => Ok("Super_L"),
            "ALT" => Ok("Alt_L"),
            "SHIFT" => Ok("Shift_L"),
            _ => anyhow::bail!("Use CTRL, LOGO, ALT or SHIFT before the shortcut key"),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(
        !modifiers.is_empty() && modifiers.len() <= 3,
        "Shortcut needs one to three modifiers"
    );
    ensure!(
        ![
            "Control_L",
            "Control_R",
            "Super_L",
            "Super_R",
            "Alt_L",
            "Alt_R",
            "Shift_L",
            "Shift_R"
        ]
        .iter()
        .any(|modifier| modifier.eq_ignore_ascii_case(key)),
        "Shortcut needs an ordinary key"
    );
    Ok((modifiers, key))
}

pub(super) async fn run(
    input: &InputSender,
    options: &DesktopOptions,
    requests: async_channel::Receiver<Insertion>,
    stop: &async_channel::Receiver<()>,
) -> anyhow::Result<()> {
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection.setup().roots[screen].root;
    let keyboard = Keyboard::load(&connection)?;
    let (dictate, cancel) = if options.external_shortcut {
        (None, None)
    } else {
        let dictate = keyboard.binding(&options.shortcut)?;
        let cancel = keyboard.binding(&options.cancel)?;
        ensure!(
            dictate != cancel,
            "Dictate and cancel shortcuts must differ"
        );
        (Some(dictate), Some(cancel))
    };
    let lock =
        keyboard.modifier("Caps_Lock").unwrap_or(0) | keyboard.modifier("Num_Lock").unwrap_or(0);
    if !options.external_shortcut {
        for (key, modifiers) in dictate.into_iter().chain(cancel) {
            for locks in [0, lock & 2, lock & !2, lock] {
                connection
                    .grab_key(
                        false,
                        root,
                        xproto::ModMask::from(modifiers | locks),
                        key,
                        xproto::GrabMode::ASYNC,
                        xproto::GrabMode::ASYNC,
                    )?
                    .check()?;
            }
        }
        use x11rb::protocol::xinput::ConnectionExt as _;
        connection.xinput_xi_query_version(2, 2)?.reply()?;
        connection
            .xinput_xi_select_events(
                root,
                &[xinput::EventMask {
                    deviceid: 1,
                    mask: vec![xinput::XIEventMask::RAW_KEY_RELEASE],
                }],
            )?
            .check()?;
    }
    connection.flush()?;
    let ready = readiness(connection.stream().as_raw_fd())?;
    let pending = requests.clone();
    let worker_input = input.clone();
    let settings = options.clone();
    let worker = std::thread::Builder::new()
        .name("linux-insertion".into())
        .spawn(move || {
            let result = (|| {
                let mut clipboard = Clipboard::new()?;
                let keyboard = Keyboard::load(&clipboard.connection)?;
                while let Ok(request) = requests.recv_blocking() {
                    let result = paste(&keyboard, &mut clipboard, &request, &settings);
                    let _ = request.reply.send_blocking(result);
                }
                Ok::<_, anyhow::Error>(())
            })();
            if let Err(error) = result {
                deliver(&worker_input, Input::Unavailable(format!("X11 input is unavailable: {error}. Check your desktop connection, then resume.")));
                worker_input.close();
            }
        })?;
    let _worker = Worker {
        input: input.clone(),
        requests: pending,
        thread: Some(worker),
    };
    let mut held = false;
    deliver(
        input,
        Input::DesktopReady {
            shortcut: if options.external_shortcut {
                "your desktop shortcut".into()
            } else {
                options.shortcut.replace("LOGO", "Super")
            },
            cancel: if options.external_shortcut {
                "your cancel shortcut".into()
            } else {
                options.cancel.replace("LOGO", "Super")
            },
        },
    );
    loop {
        while let Some(event) = connection.poll_for_event()? {
            match event {
                Event::KeyPress(key) => {
                    let modifiers = u16::from(key.state) & 0xff & !lock;
                    if cancel == Some((key.detail, modifiers)) {
                        deliver(input, Input::Cancel);
                    } else if dictate == Some((key.detail, modifiers)) && !held {
                        held = true;
                        deliver(input, Input::Press);
                    }
                    // Reserve the trigger, then return the rest of the keyboard
                    // to the focused app. XI2 observes release without a grab.
                    connection.ungrab_keyboard(key.time)?;
                    connection.flush()?;
                }
                Event::XinputRawKeyRelease(_) | Event::KeyRelease(_) if held => {
                    let Some(dictate) = dictate else {
                        continue;
                    };
                    let keys = connection.query_keymap()?.reply()?.keys;
                    let main_down = keys[usize::from(dictate.0 / 8)] & (1 << (dictate.0 % 8)) != 0;
                    if !main_down || keyboard.held(&keys) & dictate.1 != dictate.1 {
                        held = false;
                        deliver(input, Input::Release);
                    }
                }
                Event::MappingNotify(_) => anyhow::bail!(
                    "Keyboard mapping changed; resume dictation to bind the new layout"
                ),
                _ => {}
            }
        }
        tokio::select! {
            biased;
            _ = stop.recv() => break,
            _ = input.sender.closed() => break,
            readable = ready.readable() => { readable?.clear_ready(); }
        }
    }
    Ok(())
}
struct Worker {
    input: InputSender,
    requests: async_channel::Receiver<Insertion>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.input.close();
        self.requests.close();
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}
fn paste(
    keyboard: &Keyboard,
    clipboard: &mut Clipboard,
    request: &Insertion,
    options: &DesktopOptions,
) -> anyhow::Result<Inserted> {
    if !request.permit.active() {
        return Ok(Inserted::Cancelled);
    }
    if request.preserve {
        return Ok(Inserted::Unavailable(
            "Direct text input is unavailable on this X11 desktop. Turn off Keep clipboard to paste.",
        ));
    }
    let previous_owner = clipboard.owner()?;
    let locks =
        keyboard.modifier("Caps_Lock").unwrap_or(0) | keyboard.modifier("Num_Lock").unwrap_or(0);
    let started = Instant::now();
    let modifiers = loop {
        let modifiers = keyboard.held(&clipboard.connection.query_keymap()?.reply()?.keys) & !locks;
        if modifiers == 0 || started.elapsed() >= MODIFIER_WAIT || !request.permit.active() {
            break modifiers;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !request.permit.active() {
        return Ok(Inserted::Cancelled);
    }
    if clipboard.owner()? != previous_owner {
        return Ok(Inserted::Unavailable(
            "Clipboard changed while waiting for shortcut release. Dictate again when your keys are released.",
        ));
    }
    let Some(owned) = clipboard.set_text(&request.text)? else {
        return Ok(Inserted::Unavailable(
            "Clipboard changed before paste. Dictate again.",
        ));
    };
    if options.manual_paste || modifiers != 0 {
        return Ok(Inserted::Copied(
            "Text copied. Release your shortcut keys and paste into your editor.",
        ));
    }
    use x11rb::protocol::xkb::ConnectionExt as _;
    let first_layout = (|| {
        let connection = &clipboard.connection;
        ensure!(
            connection.xkb_use_extension(1, 0)?.reply()?.supported,
            "XKB is unavailable"
        );
        Ok::<_, anyhow::Error>(
            connection
                .xkb_get_state(xkb::ID::USE_CORE_KBD.into())?
                .reply()?
                .group
                == xkb::Group::M1,
        )
    })()
    .unwrap_or(false);
    let symbol =
        |name| xkbcommon::xkb::keysym_from_name(name, xkbcommon::xkb::KEYSYM_NO_FLAGS).raw();
    let control = keyboard.unshifted_code(symbol("Control_L"));
    let shift = options
        .terminal_paste
        .then(|| keyboard.unshifted_code(symbol("Shift_L")))
        .flatten();
    let v = keyboard.unshifted_code(symbol("v"));
    let (Some(control), Some(v)) = (control, v) else {
        return Ok(Inserted::Copied(
            "Text copied. This keyboard layout has no automatic paste binding; paste manually.",
        ));
    };
    if !first_layout || options.terminal_paste && shift.is_none() {
        return Ok(Inserted::Copied(
            "Text copied. Automatic paste needs your first keyboard layout; paste manually with the current layout.",
        ));
    }
    if !clipboard.owns(owned)? {
        return Ok(Inserted::Unavailable(
            "Clipboard ownership changed before paste. Dictate again.",
        ));
    }
    if !request.permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    let connection = &clipboard.connection;
    // Once committed, finish the key sequence even if cancellation arrives.
    // Always attempt releases after a partial submission failure.
    let sent = (|| {
        connection
            .xtest_fake_input(xproto::KEY_PRESS_EVENT, control, 0, 0, 0, 0, 0)?
            .check()?;
        if let Some(shift) = shift {
            connection
                .xtest_fake_input(xproto::KEY_PRESS_EVENT, shift, 0, 0, 0, 0, 0)?
                .check()?;
        }
        connection
            .xtest_fake_input(xproto::KEY_PRESS_EVENT, v, 0, 0, 0, 0, 0)?
            .check()?;
        Ok::<_, anyhow::Error>(())
    })();
    let release = |code| -> anyhow::Result<()> {
        connection
            .xtest_fake_input(xproto::KEY_RELEASE_EVENT, code, 0, 0, 0, 0, 0)?
            .check()?;
        Ok(())
    };
    let released_v = release(v);
    let released_shift = shift.map_or(Ok(()), release);
    let released_control = release(control);
    connection.flush()?;
    if sent.is_err() || released_v.is_err() || released_shift.is_err() || released_control.is_err()
    {
        return Ok(Inserted::Copied(
            "Text copied, but input submission failed. Paste manually; automatic paste was not repeated.",
        ));
    }
    Ok(Inserted::Sent)
}
fn window(handle: RawWindowHandle) -> anyhow::Result<(RustConnection, u32)> {
    let id = match handle {
        RawWindowHandle::Xcb(handle) => handle.window.get(),
        RawWindowHandle::Xlib(handle) => u32::try_from(handle.window)?,
        _ => anyhow::bail!("Linux pill requires X11 or Xwayland"),
    };
    Ok((x11rb::connect(None)?.0, id))
}
pub(super) fn configure_pill(handle: RawWindowHandle) -> anyhow::Result<()> {
    let (connection, id) = window(handle)?;
    let kind = connection
        .intern_atom(false, b"_NET_WM_WINDOW_TYPE")?
        .reply()?
        .atom;
    let notification = connection
        .intern_atom(false, b"_NET_WM_WINDOW_TYPE_NOTIFICATION")?
        .reply()?
        .atom;
    connection
        .change_property32(
            xproto::PropMode::REPLACE,
            id,
            kind,
            xproto::AtomEnum::ATOM,
            &[notification],
        )?
        .check()?;
    connection
        .change_window_attributes(
            id,
            &xproto::ChangeWindowAttributesAux::new().override_redirect(1),
        )?
        .check()?;
    let hints = connection.intern_atom(false, b"WM_HINTS")?.reply()?.atom;
    connection
        .change_property32(
            xproto::PropMode::REPLACE,
            id,
            hints,
            hints,
            &[1, 0, 0, 0, 0, 0, 0, 0, 0],
        )?
        .check()?;
    use x11rb::protocol::shape::ConnectionExt as _;
    connection
        .shape_rectangles(
            shape::SO::SET,
            shape::SK::INPUT,
            xproto::ClipOrdering::UNSORTED,
            id,
            0,
            0,
            &[],
        )?
        .check()?;
    connection.flush()?;
    Ok(())
}
pub(super) fn visible(handle: RawWindowHandle, visible: bool) -> anyhow::Result<()> {
    let (connection, id) = window(handle)?;
    if visible {
        connection.map_window(id)?.check()?;
    } else {
        connection.unmap_window(id)?.check()?;
    }
    connection.flush()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_native_descriptor_is_reported_before_borrowing_it() {
        assert!(readiness(-1).is_err());
    }

    #[test]
    fn shortcuts_require_explicit_modifiers_and_an_ordinary_key() {
        assert!(parse_binding("CTRL+LOGO+space").is_ok());
        for binding in [
            "space",
            "CTRL+",
            "CTRL+Super_L",
            "CTRL+super_l",
            "CTRL+UNKNOWN+space",
        ] {
            assert!(parse_binding(binding).is_err());
        }
    }

    #[test]
    fn paste_keys_require_an_unshifted_symbol_in_the_first_layout() {
        let keyboard = Keyboard {
            first: 8,
            columns: 4,
            symbols: vec![0x61, 0x41, 0x76, 0x56, 0x76, 0x56, 0x61, 0x41],
            modifiers: Vec::new(),
            per_modifier: 0,
        };
        assert_eq!(keyboard.unshifted_code(0x76), Some(9));
        assert_eq!(keyboard.unshifted_code(0x56), None);
    }
}
