use super::{
    DesktopOptions, Duration, Input, InputSender, InsertPermit, Inserted, Insertion, MODIFIER_WAIT,
    RawWindowHandle, deliver,
};
use anyhow::{Context, ensure};
use std::{
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
    time::Instant,
};
use tokio::io::unix::AsyncFd;
use x11rb::protocol::{
    shape::ConnectionExt as _, xinput::ConnectionExt as _, xkb::ConnectionExt as _,
};
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
// GPUI renders through X11/Xwayland on Linux. Its top-level windows publish
// _NET_WM_PID; walk parents because focus may be on a child text field.
pub(super) fn has_external_target(
    connection: &RustConnection,
    wayland: bool,
) -> anyhow::Result<bool> {
    let atom = connection.intern_atom(false, b"_NET_WM_PID")?.reply()?.atom;
    let focused = connection.get_input_focus()?.reply()?.focus;
    // Wayland may clear Xwayland focus when a native editor gains focus. None
    // then proves no app X window is focused; PointerRoot remains ambiguous.
    if wayland && focused == 0 {
        return Ok(true);
    }
    external_target(focused, std::process::id(), |window| {
        let property = connection
            .get_property(false, window, atom, xproto::AtomEnum::CARDINAL, 0, 1)?
            .reply()?;
        let pid = property.value32().and_then(|mut values| values.next());
        let parent = connection.query_tree(window)?.reply()?.parent;
        Ok((pid, parent))
    })
}
fn external_target(
    mut window: u32,
    own_pid: u32,
    mut parent: impl FnMut(u32) -> anyhow::Result<(Option<u32>, u32)>,
) -> anyhow::Result<bool> {
    // None and PointerRoot do not identify an editor. Broken or cyclic trees
    // fail closed; a native Wayland editor normally releases Xwayland focus.
    if window <= 1 {
        return Ok(false);
    }
    for _ in 0..32 {
        let (pid, next) = parent(window)?;
        if let Some(pid) = pid {
            return Ok(pid != own_pid);
        }
        if next == 0 {
            return Ok(true);
        }
        if next == window {
            return Ok(false);
        }
        window = next;
    }
    Ok(false)
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
    native: Option<arboard::Clipboard>,
    selection: u32,
}
impl Clipboard {
    pub(super) fn new() -> anyhow::Result<Self> {
        let (connection, _) = x11rb::connect(None)?;
        let selection = connection.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
        Ok(Self {
            connection,
            native: None,
            selection,
        })
    }
    fn native(&mut self) -> anyhow::Result<&mut arboard::Clipboard> {
        if self.native.is_none() {
            self.native = Some(arboard::Clipboard::new()?);
        }
        self.native
            .as_mut()
            .context("Clipboard initialization failed")
    }
    pub(super) fn owner(&self) -> anyhow::Result<u32> {
        Ok(self
            .connection
            .get_selection_owner(self.selection)?
            .reply()?
            .owner)
    }
    pub(super) fn owns(&self, owner: u32) -> anyhow::Result<bool> {
        Ok(owner != 0 && self.owner()? == owner)
    }
    pub(super) fn set_text(
        &mut self,
        text: &str,
        permit: &InsertPermit,
    ) -> anyhow::Result<Option<u32>> {
        if !permit.active() {
            return Ok(None);
        }
        self.native()?.set_text(text)?;
        let owner = self.owner()?;
        // Another application can take ownership as soon as set_text returns.
        // Verify both the payload and its stable owner before authorizing paste.
        Ok(
            (owner != 0 && self.native()?.get_text()? == text && self.owns(owner)?)
                .then_some(owner),
        )
    }
}

// Clipboard calls can wait on Xwayland or another application. Keep them off
// the portal's input thread, with one owner and one bounded command queue.
pub(super) struct ClipboardWorker {
    input: InputSender,
    requests: async_channel::Sender<ClipboardRequest>,
    ready: async_channel::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}
enum ClipboardRequest {
    Target {
        permit: InsertPermit,
        reply: async_channel::Sender<anyhow::Result<bool>>,
    },
    Set {
        text: String,
        permit: InsertPermit,
        reply: async_channel::Sender<anyhow::Result<Option<u32>>>,
    },
    Owns {
        owner: u32,
        permit: InsertPermit,
        reply: async_channel::Sender<anyhow::Result<bool>>,
    },
}
trait ClipboardAccess {
    fn external_target(&self) -> anyhow::Result<bool>;
    fn set_text(&mut self, text: &str, permit: &InsertPermit) -> anyhow::Result<Option<u32>>;
    fn owns(&self, owner: u32) -> anyhow::Result<bool>;
}
impl ClipboardAccess for Clipboard {
    fn external_target(&self) -> anyhow::Result<bool> {
        has_external_target(&self.connection, true)
    }
    fn set_text(&mut self, text: &str, permit: &InsertPermit) -> anyhow::Result<Option<u32>> {
        self.set_text(text, permit)
    }
    fn owns(&self, owner: u32) -> anyhow::Result<bool> {
        self.owns(owner)
    }
}
impl ClipboardRequest {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Cancelled callers drop their reply lanes; each native operation still validates its recording permit"
    )]
    fn perform(self, clipboard: &mut impl ClipboardAccess) {
        match self {
            ClipboardRequest::Target { permit, reply } => {
                let result = if permit.active() {
                    clipboard.external_target()
                } else {
                    Ok(false)
                };
                let _ = reply.try_send(result);
            },
            ClipboardRequest::Set {
                text,
                permit,
                reply,
            } => {
                let result = if permit.active() {
                    clipboard.set_text(&text, &permit)
                } else {
                    Ok(None)
                };
                let _ = reply.try_send(result);
            },
            ClipboardRequest::Owns {
                owner,
                permit,
                reply,
            } => {
                let result = if permit.active() {
                    clipboard.owns(owner)
                } else {
                    Ok(false)
                };
                let _ = reply.try_send(result);
            },
        }
    }
}
impl ClipboardWorker {
    pub(super) fn new(input: InputSender) -> anyhow::Result<Self> {
        Self::start(input, Clipboard::new)
    }
    fn start<C: ClipboardAccess + 'static>(
        input: InputSender,
        open: impl FnOnce() -> anyhow::Result<C> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (requests, incoming) = async_channel::bounded::<ClipboardRequest>(1);
        let (ready, started) = async_channel::bounded(1);
        let worker_input = input.clone();
        let thread = std::thread::Builder::new()
            .name("linux-clipboard".into())
            .spawn(move || {
                let result = (|| {
                    let mut clipboard = open()?;
                    #[expect(clippy::let_underscore_must_use, reason = "A cancelled desktop preparation drops its readiness receiver; input closure also retires the worker")]
                    let _ = ready.try_send(());
                    while let Ok(request) = incoming.recv_blocking() {
                        request.perform(&mut clipboard);
                    }
                    Ok::<_, anyhow::Error>(())
                })();
                if let Err(error) = result {
                    deliver(&worker_input, Input::Unavailable(format!("X11 clipboard is unavailable: {error}. Check your desktop connection, then resume.")));
                    worker_input.close();
                }
            })?;
        Ok(Self {
            input,
            requests,
            ready: started,
            thread: Some(thread),
        })
    }
    pub(super) async fn ready(&self) -> anyhow::Result<()> {
        self.ready.recv().await.context("Clipboard setup stopped")
    }
    pub(super) async fn external_target(&self, permit: InsertPermit) -> anyhow::Result<bool> {
        let (reply, result) = async_channel::bounded(1);
        self.requests
            .send(ClipboardRequest::Target { permit, reply })
            .await?;
        result.recv().await.context("Target focus check stopped")?
    }
    pub(super) async fn set_text(
        &self,
        text: String,
        permit: InsertPermit,
    ) -> anyhow::Result<Option<u32>> {
        let (reply, result) = async_channel::bounded(1);
        self.requests
            .send(ClipboardRequest::Set {
                text,
                permit,
                reply,
            })
            .await?;
        result
            .recv()
            .await
            .context("Clipboard preparation stopped")?
    }
    pub(super) async fn owns(&self, owner: u32, permit: InsertPermit) -> anyhow::Result<bool> {
        let (reply, result) = async_channel::bounded(1);
        self.requests
            .send(ClipboardRequest::Owns {
                owner,
                permit,
                reply,
            })
            .await?;
        result
            .recv()
            .await
            .context("Clipboard ownership monitoring stopped")?
    }
}
impl Drop for ClipboardWorker {
    fn drop(&mut self) {
        self.input.close();
        self.requests.close();
        if let Some(thread) = self.thread.take() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The worker has retired or panicked; a native-worker panic must not unwind this destructor"
            )]
            let _ = thread.join();
        }
    }
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
        self.first
            .checked_add(u8::try_from(position)?)
            .context("Shortcut key is outside the X11 keyboard range")
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
                if codes.iter().any(|&code| {
                    code != 0
                        && keys
                            .get(usize::from(code / 8))
                            .is_some_and(|bits| bits & (1 << (code % 8)) != 0)
                }) {
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
    let root = connection
        .setup()
        .roots
        .get(screen)
        .context("X11 display has no screen")?
        .root;
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
                    #[expect(clippy::let_underscore_must_use, reason = "Cancellation drops the waiting insertion reply; the generation permit still controls native submission")]
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
                },
                Event::XinputRawKeyRelease(_) | Event::KeyRelease(_) if held => {
                    let Some(dictate) = dictate else {
                        continue;
                    };
                    let keys = connection.query_keymap()?.reply()?.keys;
                    let main_down = keys
                        .get(usize::from(dictate.0 / 8))
                        .is_some_and(|bits| bits & (1 << (dictate.0 % 8)) != 0);
                    if !main_down || keyboard.held(&keys) & dictate.1 != dictate.1 {
                        held = false;
                        deliver(input, Input::Release);
                    }
                },
                Event::MappingNotify(_) => anyhow::bail!(
                    "Keyboard mapping changed; resume dictation to bind the new layout"
                ),
                _ => {},
            }
        }
        tokio::select! {
            biased;
            _ = stop.recv() => break,
            () = input.sender.closed() => break,
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
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The input lane is closed before joining; a worker panic must not unwind this destructor"
            )]
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
    let modifiers = wait_modifiers(options.manual_paste, &request.permit, || {
        Ok(keyboard.held(&clipboard.connection.query_keymap()?.reply()?.keys) & !locks)
    })?;
    if !request.permit.active() {
        return Ok(Inserted::Cancelled);
    }
    if clipboard.owner()? != previous_owner {
        return Ok(Inserted::Unavailable(
            "Clipboard changed while waiting for shortcut release. Dictate again when your keys are released.",
        ));
    }
    let Some(owned) = clipboard.set_text(&request.text, &request.permit)? else {
        if !request.permit.active() {
            return Ok(Inserted::Cancelled);
        }
        return Ok(Inserted::Unavailable(
            "Clipboard changed before paste. Dictate again.",
        ));
    };
    if options.manual_paste || modifiers != 0 {
        return Ok(Inserted::Copied(
            "Text copied. Release your shortcut keys and paste into your editor.",
        ));
    }
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
    if let Some(outcome) = crate::insertion::preflight(
        has_external_target(&clipboard.connection, false).unwrap_or(false),
        true,
        crate::insertion::Mode::Paste,
    ) {
        return Ok(outcome);
    }
    if !request.permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    let connection = &clipboard.connection;
    // Once committed, finish the key sequence even if cancellation arrives.
    // Always attempt releases after a partial submission failure.
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
fn wait_modifiers(
    manual: bool,
    permit: &InsertPermit,
    mut held: impl FnMut() -> anyhow::Result<u16>,
) -> anyhow::Result<u16> {
    if manual {
        return Ok(0);
    }
    let started = Instant::now();
    loop {
        let modifiers = held()?;
        if modifiers == 0 || started.elapsed() >= MODIFIER_WAIT || !permit.active() {
            return Ok(modifiers);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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
    for code in std::iter::once(control).chain(shift).chain(Some(v)) {
        if let Ok(cookie) = enqueue(xproto::KEY_PRESS_EVENT, code) {
            cookies.push(cookie);
        } else {
            failed = true;
            break;
        }
    }
    // Releases are attempted even when a press could not be enqueued. Checking
    // only after this barrier avoids round trips with synthetic modifiers held.
    for code in std::iter::once(v).chain(shift).chain(Some(control)) {
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
    #[test]
    fn focus_guard_rejects_own_child_windows_and_unknown_focus() -> anyhow::Result<()> {
        assert!(!super::external_target(20, 7, |window| Ok(
            if window == 20 {
                (None, 10)
            } else {
                (Some(7), 0)
            }
        ))?);
        assert!(super::external_target(20, 7, |_| Ok((Some(8), 0)))?);
        assert!(!super::external_target(1, 7, |_| anyhow::bail!(
            "Must not query PointerRoot"
        ))?);
        assert!(!super::external_target(20, 7, |_| Ok((None, 20)))?);
        assert!(super::external_target(20, 7, |_| anyhow::bail!("Disconnected")).is_err());
        Ok(())
    }

    use super::*;
    #[tokio::test]
    async fn blocked_focus_query_keeps_cancellation_responsive_and_never_accesses_clipboard()
    -> anyhow::Result<()> {
        struct FocusOnly {
            entered: async_channel::Sender<()>,
            release: std::sync::mpsc::Receiver<()>,
        }
        impl ClipboardAccess for FocusOnly {
            fn external_target(&self) -> anyhow::Result<bool> {
                self.entered.try_send(())?;
                self.release.recv_timeout(Duration::from_secs(2))?;
                Ok(false)
            }
            fn set_text(&mut self, _: &str, _: &InsertPermit) -> anyhow::Result<Option<u32>> {
                anyhow::bail!("Focus accessed clipboard contents")
            }
            fn owns(&self, _: u32) -> anyhow::Result<bool> {
                anyhow::bail!("Focus accessed clipboard ownership")
            }
        }
        let (sender, _events) = async_channel::bounded(4);
        let input = InputSender::new(sender);
        let permit = input.begin().context("Missing focus permit")?;
        let (entered, blocked) = async_channel::bounded(1);
        let (release, waiting) = std::sync::mpsc::channel();
        let worker = ClipboardWorker::start(input.clone(), || {
            Ok(FocusOnly {
                entered,
                release: waiting,
            })
        })?;
        worker.ready().await?;
        {
            let mut target = std::pin::pin!(worker.external_target(permit.clone()));
            tokio::select! {
                result = &mut target => anyhow::bail!("Focus did not block: {result:?}"),
                started = tokio::time::timeout(Duration::from_secs(1), blocked.recv()) => { started??; }
            }
            deliver(&input, Input::Cancel);
            assert!(!permit.active());
            release.send(())?;
            assert!(!target.await?);
            assert!(!permit.commit());
        }
        drop(worker);
        Ok(())
    }

    #[test]
    fn manual_copy_does_not_query_or_wait_for_held_modifiers() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let permit = input.begin().context("Recording")?;
        assert_eq!(
            wait_modifiers(true, &permit, || anyhow::bail!(
                "Manual copy queried a held modifier"
            ))?,
            0
        );
        let mut queried = false;
        assert_eq!(
            wait_modifiers(false, &permit, || {
                queried = true;
                Ok(0)
            })?,
            0
        );
        assert!(queried, "Automatic paste skipped modifier verification");
        Ok(())
    }

    #[test]
    #[expect(
        clippy::disallowed_types,
        reason = "Independent fake enqueue, flush, and check callbacks share only a test event log to assert native release ordering"
    )]
    fn paste_checks_once_after_all_releases_and_preserves_error_cleanup() -> anyhow::Result<()> {
        use std::cell::{Cell, RefCell};
        for shift in [None, Some(2)] {
            let events = RefCell::new(Vec::new());
            let synchronized = Cell::new(false);
            let checked = Cell::new(0);
            submit_paste(
                1,
                shift,
                3,
                |kind, code| {
                    assert!(!synchronized.get());
                    events.borrow_mut().push((kind, code));
                    Ok(())
                },
                || {
                    synchronized.set(true);
                    Ok(())
                },
                |()| {
                    assert!(synchronized.get());
                    checked.set(checked.get() + 1);
                    Ok(())
                },
            )?;
            assert_eq!(checked.get(), if shift.is_some() { 6 } else { 4 });
            assert_eq!(
                events.borrow().last(),
                Some(&(xproto::KEY_RELEASE_EVENT, 1))
            );
        }
        for fail_at in [None, Some(0), Some(2), Some(3)] {
            let events = RefCell::new(Vec::new());
            let synchronized = Cell::new(false);
            let checked = Cell::new(0);
            let queued = Cell::new(0);
            let result = submit_paste(
                1,
                Some(2),
                3,
                |kind, code| {
                    assert!(!synchronized.get());
                    let mut events = events.borrow_mut();
                    let position = events.len();
                    events.push((kind, code));
                    if fail_at == Some(position) {
                        anyhow::bail!("Synthetic enqueue failure");
                    }
                    queued.set(queued.get() + 1);
                    Ok(position)
                },
                || {
                    synchronized.set(true);
                    Ok(())
                },
                |_| {
                    assert!(
                        synchronized.get(),
                        "Checked before the sequence was released"
                    );
                    checked.set(checked.get() + 1);
                    if checked.get() == 1 {
                        anyhow::bail!("Synthetic server rejection");
                    }
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert_eq!(
                checked.get(),
                queued.get(),
                "An earlier rejection skipped later error checks"
            );
            let events = events.borrow();
            for code in [3, 2, 1] {
                assert_eq!(
                    events
                        .iter()
                        .filter(|&&event| event == (xproto::KEY_RELEASE_EVENT, code))
                        .count(),
                    1
                );
            }
            assert_eq!(
                &events[events.len() - 3..],
                &[
                    (xproto::KEY_RELEASE_EVENT, 3),
                    (xproto::KEY_RELEASE_EVENT, 2),
                    (xproto::KEY_RELEASE_EVENT, 1)
                ]
            );
            if fail_at.is_none() {
                assert_eq!(events.len(), 6);
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn clipboard_setup_does_not_block_cancel_and_skips_a_stale_queued_copy()
    -> anyhow::Result<()> {
        use futures_util::FutureExt;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct FakeClipboard(
            #[expect(
                clippy::disallowed_types,
                reason = "The test counts clipboard writes on the owned worker thread to prove cancelled requests cannot write"
            )]
            Arc<AtomicUsize>,
        );
        impl ClipboardAccess for FakeClipboard {
            fn external_target(&self) -> anyhow::Result<bool> {
                Ok(false)
            }
            fn set_text(&mut self, _: &str, permit: &InsertPermit) -> anyhow::Result<Option<u32>> {
                assert!(permit.active());
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(Some(7))
            }
            fn owns(&self, owner: u32) -> anyhow::Result<bool> {
                Ok(owner == 7)
            }
        }
        let (events, _event_receiver) = async_channel::bounded(4);
        let input = InputSender::new(events);
        let previous = input.begin().context("Previous recording")?;
        #[expect(
            clippy::disallowed_types,
            reason = "The test observes the worker’s copy count after retirement without accessing a real clipboard"
        )]
        let copies = Arc::new(AtomicUsize::new(0));
        let seen = copies.clone();
        let (entered, opening) = async_channel::bounded(1);
        let (release, waiting) = std::sync::mpsc::channel();
        let worker = ClipboardWorker::start(input.clone(), move || {
            entered.try_send(())?;
            waiting.recv_timeout(Duration::from_secs(2))?;
            Ok(FakeClipboard(seen))
        })?;
        tokio::time::timeout(Duration::from_secs(1), opening.recv()).await??;
        let mut ready = Box::pin(worker.ready());
        assert!(
            ready.as_mut().now_or_never().is_none(),
            "Desktop readiness preceded clipboard setup"
        );
        let mut previous_copy = Box::pin(worker.set_text("fixture".into(), previous.clone()));
        assert!(previous_copy.as_mut().now_or_never().is_none());
        deliver(&input, Input::Cancel);
        assert!(!previous.active());
        let current = input.begin().context("Current recording")?;
        // A local task still advances while clipboard setup waits on its own thread.
        tokio::time::timeout(
            Duration::from_millis(100),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await?;
        release.send(())?;
        ready.await?;

        assert!(previous_copy.await?.is_none());
        assert_eq!(
            worker.set_text("fixture".into(), current.clone()).await?,
            Some(7)
        );
        assert_eq!(copies.load(Ordering::Relaxed), 1);
        assert!(worker.owns(7, current).await?);
        drop(worker);
        assert!(
            input.is_closed(),
            "Clipboard retirement left its input generation active"
        );
        Ok(())
    }

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
