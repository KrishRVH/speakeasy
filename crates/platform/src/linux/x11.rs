//! X11 and Xwayland access: reserved-shortcut grabs, focus checks, clipboard ownership, and XTEST
//! paste. Blocking X calls run on owned worker threads, never on the desktop service's loop.

use std::{convert::Infallible, ops::ControlFlow, os::fd::AsFd};

use anyhow::{Context, bail, ensure};
use async_channel::{Receiver, Sender};
use raw_window_handle::RawWindowHandle;
use tokio::io::unix::AsyncFd;
use x11rb::{
    connection::Connection,
    protocol::{
        Event, shape,
        shape::ConnectionExt as _,
        xinput,
        xinput::ConnectionExt as _,
        xkb,
        xkb::ConnectionExt as _,
        xproto::{self, Atom, ConnectionExt as _, Window},
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};
use xkbcommon::xkb::{KEYSYM_CASE_INSENSITIVE, Keysym, keysym_from_name};

use super::{
    Delivery, DesktopOptions, HostSession, Input, InputSender, InsertPermit, Inserted, Insertion,
    Reply, ShortcutLabels, stopping,
};
use crate::{OwnedThread, insertion::wait_for_released_modifiers};

const MAX_FOCUS_ANCESTRY: usize = 32;
/// The eight core modifier bits; key-event state also carries pointer-button bits above them.
const CORE_MODIFIERS: u16 = 0xFF;
/// `WM_HINTS` with only the input hint set, to false: the window never takes keyboard focus.
const NO_INPUT_HINTS: [u32; 9] = [1, 0, 0, 0, 0, 0, 0, 0, 0];

struct Keyboard {
    first_keycode: u8,
    symbols_per_key: usize,
    symbols: Vec<u32>,
    modifier_keys: Vec<u8>,
    keys_per_modifier: usize,
}

impl Keyboard {
    fn load(connection: &RustConnection) -> anyhow::Result<Self> {
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

    fn keycode(&self, name: &str) -> anyhow::Result<u8> {
        let symbol = keysym_from_name(name, KEYSYM_CASE_INSENSITIVE).raw();
        ensure!(
            symbol != 0 && self.symbols_per_key != 0,
            "Unknown shortcut key"
        );
        let position = self
            .symbols
            .chunks(self.symbols_per_key)
            .position(|symbols| symbols.contains(&symbol))
            .context("Shortcut key is unavailable in this keyboard layout")?;
        self.first_keycode
            .checked_add(u8::try_from(position)?)
            .context("Shortcut key is outside the X11 keyboard range")
    }

    fn unshifted_keycode(&self, symbol: Keysym) -> Option<u8> {
        if self.symbols_per_key == 0 {
            return None;
        }
        let position = self
            .symbols
            .chunks(self.symbols_per_key)
            .position(|symbols| symbols.first() == Some(&symbol.raw()))?;
        self.first_keycode.checked_add(u8::try_from(position).ok()?)
    }

    fn modifier_mask(&self, name: &str) -> anyhow::Result<u16> {
        let code = self.keycode(name)?;
        ensure!(
            self.keys_per_modifier != 0,
            "Keyboard has no modifier mapping"
        );
        let group = self
            .modifier_keys
            .chunks(self.keys_per_modifier)
            .position(|codes| codes.contains(&code))
            .context("Shortcut modifier is not mapped")?;
        Ok(1 << group)
    }

    fn lock_modifiers(&self) -> u16 {
        self.modifier_mask("Caps_Lock").unwrap_or(0) | self.modifier_mask("Num_Lock").unwrap_or(0)
    }

    fn held_modifiers(&self, keys: &[u8; 32]) -> u16 {
        if self.keys_per_modifier == 0 {
            return 0;
        }
        self.modifier_keys
            .chunks(self.keys_per_modifier)
            .enumerate()
            .filter(|(_, codes)| {
                codes
                    .iter()
                    .any(|&code| code != 0 && is_key_down(keys, code))
            })
            .fold(0, |held, (group, _)| held | (1 << group))
    }

    fn binding(&self, expression: &str) -> anyhow::Result<KeyBinding> {
        let (modifier_names, key) = parse_binding(expression)?;
        let mut modifiers = 0;
        for name in modifier_names {
            modifiers |= self.modifier_mask(name)?;
        }
        Ok(KeyBinding {
            key: self.keycode(key)?,
            modifiers,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct KeyBinding {
    key: u8,
    modifiers: u16,
}

/// The reserved dictate and cancel chords.
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

    fn grab(self, connection: &RustConnection, root: Window, lock: u16) -> anyhow::Result<()> {
        // Passive grabs match the exact modifier state, so each Caps and Num Lock combination needs
        // its own grab.
        let caps = u16::from(xproto::ModMask::LOCK);
        let lock_states = [0, lock & caps, lock & !caps, lock];
        for binding in [self.dictate, self.cancel] {
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
                    .check()?;
            }
        }
        Ok(())
    }
}

/// Turns grabbed shortcut presses and observed releases into dictation input.
struct ShortcutTracker<'a> {
    connection: &'a RustConnection,
    keyboard: &'a Keyboard,
    shortcuts: Option<Shortcuts>,
    lock: u16,
    input: &'a InputSender,
    held: bool,
}

impl ShortcutTracker<'_> {
    fn observe(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::KeyPress(key) => self.press(&key),
            Event::XinputRawKeyRelease(_) | Event::KeyRelease(_) if self.held => self.release(),
            Event::MappingNotify(_) => {
                bail!("Keyboard mapping changed; resume dictation to bind the new layout")
            },
            _ => Ok(()),
        }
    }

    fn press(&mut self, key: &xproto::KeyPressEvent) -> anyhow::Result<()> {
        let pressed = KeyBinding {
            key: key.detail,
            modifiers: u16::from(key.state) & CORE_MODIFIERS & !self.lock,
        };
        if let Some(shortcuts) = self.shortcuts {
            if shortcuts.cancel == pressed {
                self.input.deliver(Input::Cancel);
            } else if shortcuts.dictate == pressed && !self.held {
                self.held = true;
                self.input.deliver(Input::Press);
            }
        }
        // The grab reserves only the trigger: return the keyboard to the focused app, and let XI2
        // raw events observe the release without a grab.
        self.connection.ungrab_keyboard(key.time)?;
        self.connection.flush()?;
        Ok(())
    }

    fn release(&mut self) -> anyhow::Result<()> {
        let Some(Shortcuts { dictate, .. }) = self.shortcuts else {
            return Ok(());
        };
        let keys = self.connection.query_keymap()?.reply()?.keys;
        if !is_key_down(&keys, dictate.key)
            || self.keyboard.held_modifiers(&keys) & dictate.modifiers != dictate.modifiers
        {
            self.held = false;
            self.input.deliver(Input::Release);
        }
        Ok(())
    }
}

/// Serializes X11 insertion on its own thread. Dropping it revokes the current permit and closes its
/// queue, then joins the thread; the desktop thread closes the input lane after reporting why it
/// stopped.
struct InsertionWorker {
    _thread: OwnedThread,
    input: InputSender,
    requests: Receiver<Insertion>,
}

impl InsertionWorker {
    fn spawn(
        input: InputSender,
        options: DesktopOptions,
        requests: Receiver<Insertion>,
    ) -> anyhow::Result<Self> {
        let thread = OwnedThread::spawn("x11-insertion", {
            let (input, requests) = (input.clone(), requests.clone());
            move || {
                if let Err(error) = serve_insertions(&requests, &options) {
                    input.fail(format!(
                        "X11 input is unavailable: {error}. Check your desktop connection, then resume."
                    ));
                }
            }
        })?;
        Ok(Self {
            _thread: thread,
            input,
            requests,
        })
    }
}

impl Drop for InsertionWorker {
    fn drop(&mut self) {
        self.input.cancel();
        self.requests.close();
    }
}

/// Clipboard and focus calls can wait on Xwayland or another client, so they run on one owned
/// thread behind a bounded queue, away from the desktop service's input loop.
pub(super) struct ClipboardWorker {
    _thread: OwnedThread,
    input: InputSender,
    requests: Sender<ClipboardRequest>,
    ready: Receiver<()>,
}

impl ClipboardWorker {
    pub(super) fn spawn(input: InputSender) -> anyhow::Result<Self> {
        Self::spawn_with(input, Clipboard::open)
    }

    fn spawn_with<C: ClipboardAccess + 'static>(
        input: InputSender,
        open: impl FnOnce() -> anyhow::Result<C> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (requests, incoming) = async_channel::bounded::<ClipboardRequest>(1);
        let (opened, ready) = Reply::channel();
        let thread = OwnedThread::spawn("x11-clipboard", {
            let input = input.clone();
            // `opened` drops only after a failure is reported, so that report precedes the desktop
            // loop's generic setup failure.
            move || match open() {
                Ok(clipboard) => {
                    opened.send(());
                    serve_clipboard(clipboard, &incoming);
                },
                Err(error) => {
                    input.fail(format!(
                        "X11 clipboard is unavailable: {error}. Check your desktop connection, then resume."
                    ));
                },
            }
        })?;
        Ok(Self {
            _thread: thread,
            input,
            requests,
            ready,
        })
    }

    pub(super) async fn ready(&self) -> anyhow::Result<()> {
        self.ready.recv().await.context("Clipboard setup stopped")
    }

    pub(super) async fn external_target(&self, permit: InsertPermit) -> anyhow::Result<bool> {
        self.ask(
            |reply| ClipboardRequest::Target { permit, reply },
            "Target focus check stopped",
        )
        .await
    }

    pub(super) async fn set_text(
        &self,
        text: String,
        permit: InsertPermit,
    ) -> anyhow::Result<Option<Window>> {
        self.ask(
            |reply| ClipboardRequest::Set {
                text,
                permit,
                reply,
            },
            "Clipboard preparation stopped",
        )
        .await
    }

    pub(super) async fn owns(&self, owner: Window, permit: InsertPermit) -> anyhow::Result<bool> {
        self.ask(
            |reply| ClipboardRequest::Owns {
                owner,
                permit,
                reply,
            },
            "Clipboard ownership monitoring stopped",
        )
        .await
    }

    async fn ask<T>(
        &self,
        request: impl FnOnce(Reply<anyhow::Result<T>>) -> ClipboardRequest,
        stopped: &'static str,
    ) -> anyhow::Result<T> {
        let (reply, answer) = Reply::channel();
        self.requests.send(request(reply)).await?;
        answer.recv().await.context(stopped)?
    }
}

impl Drop for ClipboardWorker {
    fn drop(&mut self) {
        self.input.cancel();
        self.requests.close();
    }
}

enum ClipboardRequest {
    Target {
        permit: InsertPermit,
        reply: Reply<anyhow::Result<bool>>,
    },
    Set {
        text: String,
        permit: InsertPermit,
        reply: Reply<anyhow::Result<Option<Window>>>,
    },
    Owns {
        owner: Window,
        permit: InsertPermit,
        reply: Reply<anyhow::Result<bool>>,
    },
}

impl ClipboardRequest {
    fn perform(self, clipboard: &mut impl ClipboardAccess) {
        match self {
            Self::Target { permit, reply } => {
                let target = if permit.active() {
                    clipboard.external_target()
                } else {
                    Ok(false)
                };
                reply.send(target);
            },
            Self::Set {
                text,
                permit,
                reply,
            } => {
                let owner = if permit.active() {
                    clipboard.set_text(&text)
                } else {
                    Ok(None)
                };
                reply.send(owner);
            },
            Self::Owns {
                owner,
                permit,
                reply,
            } => {
                let owned = if permit.active() {
                    clipboard.owns(owner)
                } else {
                    Ok(false)
                };
                reply.send(owned);
            },
        }
    }
}

trait ClipboardAccess {
    fn external_target(&self) -> anyhow::Result<bool>;
    fn set_text(&mut self, text: &str) -> anyhow::Result<Option<Window>>;
    fn owns(&self, owner: Window) -> anyhow::Result<bool>;
}

struct Clipboard {
    connection: RustConnection,
    /// Opened on the first copy, so a worker that only checks focus never touches clipboard
    /// contents.
    native: Option<arboard::Clipboard>,
    selection: Atom,
}

impl Clipboard {
    fn open() -> anyhow::Result<Self> {
        let (connection, _) = x11rb::connect(None)?;
        let selection = intern(&connection, b"CLIPBOARD")?;
        Ok(Self {
            connection,
            native: None,
            selection,
        })
    }

    fn native(&mut self) -> anyhow::Result<&mut arboard::Clipboard> {
        Ok(match &mut self.native {
            Some(native) => native,
            empty => empty.insert(arboard::Clipboard::new()?),
        })
    }

    fn owner(&self) -> anyhow::Result<Window> {
        Ok(self
            .connection
            .get_selection_owner(self.selection)?
            .reply()?
            .owner)
    }
}

impl ClipboardAccess for Clipboard {
    fn external_target(&self) -> anyhow::Result<bool> {
        has_external_target(&self.connection, HostSession::Wayland)
    }

    fn set_text(&mut self, text: &str) -> anyhow::Result<Option<Window>> {
        self.native()?.set_text(text)?;
        // Another client can take the selection as soon as the copy returns, so paste needs both
        // the payload and an owner that stays put.
        let owner = self.owner()?;
        Ok(
            (owner != x11rb::NONE && self.native()?.get_text()? == text && self.owns(owner)?)
                .then_some(owner),
        )
    }

    fn owns(&self, owner: Window) -> anyhow::Result<bool> {
        Ok(owner != x11rb::NONE && self.owner()? == owner)
    }
}

pub(super) async fn run(
    input: &InputSender,
    options: &DesktopOptions,
    requests: Receiver<Insertion>,
    stop: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    let (connection, screen) = x11rb::connect(None)?;
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
    if let Some(shortcuts) = shortcuts {
        shortcuts.grab(&connection, root, lock)?;
        observe_releases(&connection, root)?;
    }
    connection.flush()?;
    let readiness = AsyncFd::new(connection.stream().as_fd().try_clone_to_owned()?)?;
    let _worker = InsertionWorker::spawn(input.clone(), options.clone(), requests)?;
    input.deliver(ShortcutLabels::new(options).into_desktop_ready());
    let mut tracker = ShortcutTracker {
        connection: &connection,
        keyboard: &keyboard,
        shortcuts,
        lock,
        input,
        held: false,
    };
    loop {
        while let Some(event) = connection.poll_for_event()? {
            tracker.observe(event)?;
        }
        tokio::select! {
            biased;
            () = stopping(stop, input) => break,
            readable = readiness.readable() => readable?.clear_ready(),
        }
    }
    Ok(())
}

/// Subscribes `root` to XI2 raw key releases, which arrive without a keyboard grab.
fn observe_releases(connection: &RustConnection, root: Window) -> anyhow::Result<()> {
    // XI2 events flow only after the client announces its version; the reply carries nothing we
    // need.
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

fn serve_clipboard(mut clipboard: impl ClipboardAccess, requests: &Receiver<ClipboardRequest>) {
    while let Ok(request) = requests.recv_blocking() {
        request.perform(&mut clipboard);
    }
}

fn serve_insertions(
    requests: &Receiver<Insertion>,
    options: &DesktopOptions,
) -> anyhow::Result<()> {
    let mut clipboard = Clipboard::open()?;
    let keyboard = Keyboard::load(&clipboard.connection)?;
    while let Ok(request) = requests.recv_blocking() {
        let outcome = paste(&keyboard, &mut clipboard, &request, options);
        request.reply.send(outcome);
    }
    Ok(())
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
    if request.delivery == Delivery::Direct {
        return Ok(Inserted::Unavailable(
            "Direct text input is unavailable on this X11 desktop. Turn off Keep clipboard to paste.",
        ));
    }
    match copy_after_release(keyboard, clipboard, request, options)? {
        ControlFlow::Continue(owner) => paste_copied(keyboard, clipboard, request, options, owner),
        ControlFlow::Break(outcome) => Ok(outcome),
    }
}

fn copy_after_release(
    keyboard: &Keyboard,
    clipboard: &mut Clipboard,
    request: &Insertion,
    options: &DesktopOptions,
) -> anyhow::Result<ControlFlow<Inserted, Window>> {
    let previous_owner = clipboard.owner()?;
    let shortcut_held = !options.manual_paste && {
        let locks = keyboard.lock_modifiers();
        wait_for_released_modifiers(&request.permit, || {
            let keys = clipboard.connection.query_keymap()?.reply()?.keys;
            Ok(keyboard.held_modifiers(&keys) & !locks != 0)
        })?
    };
    let owner_changed = clipboard.owner()? != previous_owner;
    // Checked after that round trip, so a cancel during it never reaches the clipboard.
    if !request.permit.active() {
        return Ok(ControlFlow::Break(Inserted::Cancelled));
    }
    if owner_changed {
        return Ok(ControlFlow::Break(Inserted::Unavailable(
            "Clipboard changed while waiting for shortcut release. Dictate again when your keys are released.",
        )));
    }
    let Some(owner) = clipboard.set_text(&request.text)? else {
        return Ok(ControlFlow::Break(if request.permit.active() {
            Inserted::Unavailable("Clipboard changed before paste. Dictate again.")
        } else {
            Inserted::Cancelled
        }));
    };
    if options.manual_paste || shortcut_held {
        return Ok(ControlFlow::Break(Inserted::Copied(
            "Text copied. Release your shortcut keys and paste into your editor.",
        )));
    }
    Ok(ControlFlow::Continue(owner))
}

fn paste_copied(
    keyboard: &Keyboard,
    clipboard: &Clipboard,
    request: &Insertion,
    options: &DesktopOptions,
    owner: Window,
) -> anyhow::Result<Inserted> {
    let first_layout = uses_first_layout(&clipboard.connection).unwrap_or(false);
    let control = keyboard.unshifted_keycode(Keysym::Control_L);
    let shift = if options.terminal_paste {
        keyboard.unshifted_keycode(Keysym::Shift_L)
    } else {
        None
    };
    let v = keyboard.unshifted_keycode(Keysym::v);
    let (Some(control), Some(v)) = (control, v) else {
        return Ok(Inserted::Copied(
            "Text copied. This keyboard layout has no automatic paste binding; paste manually.",
        ));
    };
    if !first_layout || (options.terminal_paste && shift.is_none()) {
        return Ok(Inserted::Copied(
            "Text copied. Automatic paste needs your first keyboard layout; paste manually with the current layout.",
        ));
    }
    if !clipboard.owns(owner)? {
        return Ok(Inserted::Unavailable(
            "Clipboard ownership changed before paste. Dictate again.",
        ));
    }
    let external = has_external_target(&clipboard.connection, HostSession::X11).unwrap_or(false);
    if let Some(outcome) = Delivery::Paste.check_focus(external) {
        return Ok(outcome);
    }
    if !request.permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    // Once committed, the key sequence completes even if cancellation arrives.
    let connection = &clipboard.connection;
    if submit_paste(
        control,
        shift,
        v,
        |kind, code| Ok(connection.xtest_fake_input(kind, code, 0, 0, 0, 0, 0)?),
        || {
            connection.get_input_focus()?.reply()?;
            Ok(())
        },
        |cookie| Ok(cookie.check()?),
    )
    .is_err()
    {
        return Ok(Inserted::Copied(
            "Text copied, but input submission failed. Paste manually; automatic paste was not repeated.",
        ));
    }
    Ok(Inserted::Sent)
}

fn uses_first_layout(connection: &RustConnection) -> anyhow::Result<bool> {
    ensure!(
        connection.xkb_use_extension(1, 0)?.reply()?.supported,
        "XKB is unavailable"
    );
    let state = connection
        .xkb_get_state(xkb::ID::USE_CORE_KBD.into())?
        .reply()?;
    Ok(state.group == xkb::Group::M1)
}

fn submit_paste<C>(
    control: u8,
    shift: Option<u8>,
    v: u8,
    mut enqueue: impl FnMut(u8, u8) -> anyhow::Result<C>,
    synchronize: impl FnOnce() -> anyhow::Result<()>,
    mut check: impl FnMut(C) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut cookies = Vec::with_capacity(6);
    let mut failed = false;
    for code in [Some(control), shift, Some(v)].into_iter().flatten() {
        let Ok(cookie) = enqueue(xproto::KEY_PRESS_EVENT, code) else {
            failed = true;
            break;
        };
        cookies.push(cookie);
    }
    // Releases follow even a failed press. Checks wait for the barrier: a round trip per event
    // would hold synthetic modifiers down.
    for code in [Some(v), shift, Some(control)].into_iter().flatten() {
        match enqueue(xproto::KEY_RELEASE_EVENT, code) {
            Ok(cookie) => cookies.push(cookie),
            Err(_) => failed = true,
        }
    }
    synchronize()?;
    for cookie in cookies {
        failed |= check(cookie).is_err();
    }
    ensure!(!failed, "Input submission failed");
    Ok(())
}

fn is_key_down(keys: &[u8; 32], code: u8) -> bool {
    keys.get(usize::from(code / 8))
        .is_some_and(|bits| bits & (1 << (code % 8)) != 0)
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
            _ => bail!("Use CTRL, LOGO, ALT or SHIFT before the shortcut key"),
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

fn has_external_target(connection: &RustConnection, host: HostSession) -> anyhow::Result<bool> {
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

/// Whether focus belongs to another process. GPUI publishes `_NET_WM_PID` on its top-level
/// windows, so the walk climbs from a focused child field to the first window that names a PID.
fn focus_is_external(
    host: HostSession,
    mut window: Window,
    own_pid: u32,
    mut inspect: impl FnMut(Window) -> anyhow::Result<(Option<u32>, Window)>,
) -> anyhow::Result<bool> {
    if window == x11rb::NONE {
        // Wayland clears Xwayland focus when a native editor takes it; on X11 it names no editor.
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

pub(super) fn configure_pill(handle: RawWindowHandle) -> anyhow::Result<()> {
    let (connection, window) = connect_to_window(handle)?;
    let window_type = intern(&connection, b"_NET_WM_WINDOW_TYPE")?;
    let notification = intern(&connection, b"_NET_WM_WINDOW_TYPE_NOTIFICATION")?;
    connection
        .change_property32(
            xproto::PropMode::REPLACE,
            window,
            window_type,
            xproto::AtomEnum::ATOM,
            &[notification],
        )?
        .check()?;
    connection
        .change_window_attributes(
            window,
            &xproto::ChangeWindowAttributesAux::new().override_redirect(1),
        )?
        .check()?;
    connection
        .change_property32(
            xproto::PropMode::REPLACE,
            window,
            xproto::AtomEnum::WM_HINTS,
            xproto::AtomEnum::WM_HINTS,
            &NO_INPUT_HINTS,
        )?
        .check()?;
    // An empty input region lets clicks pass through.
    connection
        .shape_rectangles(
            shape::SO::SET,
            shape::SK::INPUT,
            xproto::ClipOrdering::UNSORTED,
            window,
            0,
            0,
            &[],
        )?
        .check()?;
    connection.flush()?;
    Ok(())
}

pub(super) fn set_visible(handle: RawWindowHandle, visible: bool) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "The owned window may already be destroyed or its display disconnected during presentation teardown"
    )]
    let _ = change_visibility(handle, visible);
}

fn change_visibility(handle: RawWindowHandle, visible: bool) -> anyhow::Result<()> {
    let (connection, window) = connect_to_window(handle)?;
    if visible {
        connection.map_window(window)?.check()?;
    } else {
        connection.unmap_window(window)?.check()?;
    }
    connection.flush()?;
    Ok(())
}

fn connect_to_window(handle: RawWindowHandle) -> anyhow::Result<(RustConnection, Window)> {
    let window = match handle {
        RawWindowHandle::Xcb(handle) => handle.window.get(),
        RawWindowHandle::Xlib(handle) => u32::try_from(handle.window)?,
        _ => bail!("Linux pill requires X11 or Xwayland"),
    };
    Ok((x11rb::connect(None)?.0, window))
}

fn intern(connection: &RustConnection, name: &[u8]) -> anyhow::Result<Atom> {
    Ok(connection.intern_atom(false, name)?.reply()?.atom)
}

#[cfg(test)]
mod tests {
    use std::{mem, sync::mpsc, time::Duration};

    use futures_util::FutureExt;

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

    #[tokio::test]
    async fn blocked_focus_query_keeps_cancellation_responsive_and_never_accesses_clipboard()
    -> anyhow::Result<()> {
        struct FocusOnly {
            entered: async_channel::Sender<()>,
            release: mpsc::Receiver<()>,
        }

        impl ClipboardAccess for FocusOnly {
            fn external_target(&self) -> anyhow::Result<bool> {
                self.entered.try_send(())?;
                self.release.recv_timeout(Duration::from_secs(2))?;
                Ok(false)
            }

            fn set_text(&mut self, _: &str) -> anyhow::Result<Option<Window>> {
                bail!("Focus accessed clipboard contents")
            }

            fn owns(&self, _: Window) -> anyhow::Result<bool> {
                bail!("Focus accessed clipboard ownership")
            }
        }

        let (sender, _events) = async_channel::bounded(4);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Missing focus permit")?;
        let (entered, blocked) = async_channel::bounded(1);
        let (release, waiting) = mpsc::channel();
        let worker = ClipboardWorker::spawn_with(input.clone(), || {
            Ok(FocusOnly {
                entered,
                release: waiting,
            })
        })?;
        worker.ready().await?;
        {
            let mut target = std::pin::pin!(worker.external_target(permit.clone()));
            tokio::select! {
                result = &mut target => bail!("Focus did not block: {result:?}"),
                started = tokio::time::timeout(Duration::from_secs(1), blocked.recv()) => {
                    started??;
                },
            }
            input.deliver(Input::Cancel);
            assert!(!permit.active());
            release.send(())?;
            assert!(!target.await?);
            assert!(!permit.commit());
        }
        drop(worker);
        Ok(())
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Step {
        Enqueue { kind: u8, code: u8, accepted: bool },
        Synchronize,
        Check,
    }

    struct PasteTrace {
        result: anyhow::Result<()>,
        enqueued: Vec<(u8, u8, bool)>,
        checks: usize,
    }

    /// Runs `submit_paste` against fakes that refuse the enqueue at `fail_at` and, when asked,
    /// the first check, then proves every enqueue precedes the barrier and every check follows it.
    fn trace_paste(
        shift: Option<u8>,
        fail_at: Option<usize>,
        reject_first_check: bool,
    ) -> PasteTrace {
        let (log, steps) = mpsc::channel();
        let (enqueue_log, synchronize_log) = (log.clone(), log.clone());
        let mut positions = 0..;
        let mut reject_check = reject_first_check;
        let result = submit_paste(
            1,
            shift,
            3,
            move |kind, code| {
                let accepted = positions.next() != fail_at;
                enqueue_log.send(Step::Enqueue {
                    kind,
                    code,
                    accepted,
                })?;
                ensure!(accepted, "Synthetic enqueue failure");
                Ok(())
            },
            move || Ok(synchronize_log.send(Step::Synchronize)?),
            move |()| {
                log.send(Step::Check)?;
                ensure!(!mem::take(&mut reject_check), "Synthetic server rejection");
                Ok(())
            },
        );
        let steps: Vec<_> = steps.try_iter().collect();
        let barrier = steps
            .iter()
            .position(|step| *step == Step::Synchronize)
            .expect("Paste never synchronized");
        let (before_barrier, from_barrier) = steps.split_at(barrier);
        let checks = &from_barrier[1..];
        assert!(
            checks.iter().all(|step| *step == Step::Check),
            "Enqueued after the synchronization barrier"
        );
        let enqueued = before_barrier
            .iter()
            .map(|step| match *step {
                Step::Enqueue {
                    kind,
                    code,
                    accepted,
                } => (kind, code, accepted),
                Step::Synchronize | Step::Check => {
                    panic!("Checked before the sequence was released")
                },
            })
            .collect();
        PasteTrace {
            result,
            enqueued,
            checks: checks.len(),
        }
    }

    #[test]
    fn paste_checks_once_after_all_releases_and_preserves_error_cleanup() {
        let release = |code| (xproto::KEY_RELEASE_EVENT, code);
        for shift in [None, Some(2)] {
            let trace = trace_paste(shift, None, false);
            assert!(trace.result.is_ok());
            assert_eq!(trace.checks, if shift.is_some() { 6 } else { 4 });
            assert_eq!(
                trace.enqueued.last().map(|&(kind, code, _)| (kind, code)),
                Some(release(1))
            );
        }
        for fail_at in [None, Some(0), Some(2), Some(3)] {
            let trace = trace_paste(Some(2), fail_at, true);
            assert!(trace.result.is_err());
            let accepted = trace
                .enqueued
                .iter()
                .filter(|&&(.., accepted)| accepted)
                .count();
            assert_eq!(
                trace.checks, accepted,
                "An earlier rejection skipped later error checks"
            );
            let attempted: Vec<_> = trace
                .enqueued
                .iter()
                .map(|&(kind, code, _)| (kind, code))
                .collect();
            for code in [3, 2, 1] {
                assert_eq!(
                    attempted
                        .iter()
                        .filter(|&&event| event == release(code))
                        .count(),
                    1
                );
            }
            assert_eq!(
                attempted[attempted.len() - 3..],
                [release(3), release(2), release(1)]
            );
            if fail_at.is_none() {
                assert_eq!(attempted.len(), 6);
            }
        }
    }

    #[tokio::test]
    async fn clipboard_setup_does_not_block_cancel_and_skips_a_stale_queued_copy()
    -> anyhow::Result<()> {
        struct FakeClipboard(mpsc::Sender<()>);

        impl ClipboardAccess for FakeClipboard {
            fn external_target(&self) -> anyhow::Result<bool> {
                Ok(false)
            }

            fn set_text(&mut self, _: &str) -> anyhow::Result<Option<Window>> {
                self.0.send(())?;
                Ok(Some(7))
            }

            fn owns(&self, owner: Window) -> anyhow::Result<bool> {
                Ok(owner == 7)
            }
        }

        let (events, _event_receiver) = async_channel::bounded(4);
        let input = InputSender::new(events);
        let previous = input.begin().context("Previous recording")?;
        let (copy_log, copies) = mpsc::channel();
        let (entered, opening) = async_channel::bounded(1);
        let (release, waiting) = mpsc::channel();
        let worker = ClipboardWorker::spawn_with(input.clone(), move || {
            entered.try_send(())?;
            waiting.recv_timeout(Duration::from_secs(2))?;
            Ok(FakeClipboard(copy_log))
        })?;
        tokio::time::timeout(Duration::from_secs(1), opening.recv()).await??;
        let mut ready = Box::pin(worker.ready());
        assert!(
            ready.as_mut().now_or_never().is_none(),
            "Desktop readiness preceded clipboard setup"
        );
        let mut previous_copy = Box::pin(worker.set_text("fixture".into(), previous.clone()));
        assert!(previous_copy.as_mut().now_or_never().is_none());
        input.deliver(Input::Cancel);
        assert!(!previous.active());
        let current = input.begin().context("Current recording")?;
        tokio::time::timeout(
            Duration::from_millis(100),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .context("Clipboard setup blocked the local runtime")?;
        release.send(())?;
        ready.await?;

        assert!(previous_copy.await?.is_none());
        assert_eq!(
            worker.set_text("fixture".into(), current.clone()).await?,
            Some(7)
        );
        assert_eq!(copies.try_iter().count(), 1);
        assert!(worker.owns(7, current.clone()).await?);
        drop(worker);
        assert!(
            !current.active(),
            "Clipboard retirement left its input generation active"
        );
        assert!(
            !input.is_closed(),
            "Clipboard retirement closed input before the desktop thread could report why"
        );
        Ok(())
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
