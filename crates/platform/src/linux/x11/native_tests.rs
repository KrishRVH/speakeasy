use std::{
    io::{BufRead as _, BufReader},
    process::{Child, Command, Stdio},
    time::Duration,
};

use anyhow::{Context as _, bail, ensure};
use x11rb::{
    connection::{Connection as _, RequestConnection as _},
    protocol::{
        Event, xfixes,
        xproto::{self, ConnectionExt as _},
        xtest::ConnectionExt as _,
    },
    xcb_ffi::XCBConnection,
};
use xkbcommon::xkb::Keysym;

use super::{
    Keyboard,
    clipboard::{Clipboard, ClipboardAccess as _},
    connect_with_root, intern,
};
use crate::{DesktopOptions, Input, InputSender};

const PRIVATE_DISPLAY: &str = "SPEAKEASY_TEST_PRIVATE_DISPLAY";

#[tokio::test]
#[ignore = "Injects XTEST keys and remaps a key on a private Xvfb display it starts, never the current one"]
async fn a_keyboard_switch_keeps_shortcuts_and_a_remap_stops_them() -> anyhow::Result<()> {
    if !on_private_display(
        "a_keyboard_switch_keeps_shortcuts_and_a_remap_stops_them",
        &[],
    )? {
        return Ok(());
    }
    let (events, received) = async_channel::bounded(8);
    let (_desktop, _monitor, _inserter) = crate::linux::start(
        InputSender::new(events),
        DesktopOptions::default(),
        crate::FocusReport::default().subscribe(),
    )?;
    let next = async || {
        tokio::time::timeout(Duration::from_secs(3), received.recv())
            .await?
            .context("Desktop input closed")
    };
    let ready = next().await?;
    ensure!(
        matches!(ready, Input::DesktopReady { .. }),
        "Desktop setup gave {ready:?}"
    );
    let (connection, _) = XCBConnection::connect(None)?;
    let keyboard = Keyboard::load(&connection)?;
    let key = |kind, symbol| -> anyhow::Result<()> {
        let code = keyboard.keycode(symbol)?;
        connection
            .xtest_fake_input(kind, code, 0, 0, 0, 0, 0)?
            .check()?;
        Ok(())
    };

    for symbol in [Keysym::Control_L, Keysym::Super_L, Keysym::space] {
        key(xproto::KEY_PRESS_EVENT, symbol)?;
    }

    let mut switched = false;
    while let Some(event) = connection.poll_for_event()? {
        switched |= matches!(event, Event::MappingNotify(_));
    }
    ensure!(switched, "The server announced no keyboard switch");
    let pressed = next().await?;
    ensure!(
        matches!(pressed, Input::Press),
        "The chord after a keyboard switch gave {pressed:?}"
    );
    key(xproto::KEY_RELEASE_EVENT, Keysym::space)?;
    let released = next().await?;
    ensure!(
        matches!(released, Input::Release),
        "Releasing the chord gave {released:?}"
    );
    for symbol in [Keysym::Super_L, Keysym::Control_L] {
        key(xproto::KEY_RELEASE_EVENT, symbol)?;
    }

    let code = connection.setup().max_keycode;
    let current = connection.get_keyboard_mapping(code, 1)?.reply()?;
    let remapped = vec![Keysym::F35.raw(); current.keysyms.len()];
    ensure!(remapped != current.keysyms, "The remap changes nothing");
    connection
        .change_keyboard_mapping(1, code, current.keysyms_per_keycode, &remapped)?
        .check()?;
    let stopped = next()
        .await
        .context("A remap left shortcuts bound to the old mapping")?;
    let Input::Unavailable(reason) = &stopped else {
        bail!("A remap gave {stopped:?}");
    };
    ensure!(
        reason.starts_with("Keyboard mapping changed. Resume dictation"),
        "A remap stopped input for another reason: {reason}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Grabs a key on a private Xvfb display it starts, never the current one"]
async fn a_shortcut_another_app_holds_stops_input_naming_the_shortcut() -> anyhow::Result<()> {
    if !on_private_display(
        "a_shortcut_another_app_holds_stops_input_naming_the_shortcut",
        &[],
    )? {
        return Ok(());
    }
    let (connection, root) = connect_with_root()?;
    let options = DesktopOptions::default();
    let held = Keyboard::load(&connection)?.binding(&options.shortcut)?;
    connection
        .grab_key(
            false,
            root,
            xproto::ModMask::from(held.modifiers),
            held.key,
            xproto::GrabMode::ASYNC,
            xproto::GrabMode::ASYNC,
        )?
        .check()?;
    let (events, received) = async_channel::bounded(8);
    let (_desktop, _monitor, _inserter) = crate::linux::start(
        InputSender::new(events),
        options,
        crate::FocusReport::default().subscribe(),
    )?;
    let stopped = tokio::time::timeout(Duration::from_secs(3), received.recv())
        .await?
        .context("Desktop input closed")?;
    let Input::Unavailable(reason) = &stopped else {
        bail!("A held shortcut gave {stopped:?}");
    };
    ensure!(
        reason.starts_with("Another app already uses the dictation shortcut. Free it there"),
        "A held shortcut stopped input for another reason: {reason}"
    );
    Ok(())
}

#[test]
#[ignore = "Writes the clipboard of a private Xvfb display it starts, never the current one"]
fn a_fresh_clipboard_owns_its_first_copy() -> anyhow::Result<()> {
    if !on_private_display("a_fresh_clipboard_owns_its_first_copy", &[])? {
        return Ok(());
    }
    let (connection, other) = selection_owner()?;
    let selection = intern(&connection, b"CLIPBOARD")?;
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    let permit = input.begin().context("Clipboard recording")?;

    for _ in 0..100 {
        connection
            .set_selection_owner(other, selection, x11rb::CURRENT_TIME)?
            .check()?;
        ensure!(
            Clipboard::open()?.set_text("fixture", &permit)?.is_some(),
            "A first copy was refused as changed before paste"
        );
    }
    Ok(())
}

#[test]
#[ignore = "Copies on a private Xvfb display it starts, never the current one"]
fn an_owner_that_keeps_its_window_but_changes_its_text_no_longer_holds_the_copy()
-> anyhow::Result<()> {
    if !on_private_display(
        "an_owner_that_keeps_its_window_but_changes_its_text_no_longer_holds_the_copy",
        &[],
    )? {
        return Ok(());
    }
    let (events, _received) = async_channel::bounded(1);
    let input = InputSender::new(events);
    let permit = input.begin().context("Recording")?;
    let mut clipboard = Clipboard::open()?;
    let owner = clipboard
        .set_text("fixture", &permit)?
        .context("The fixture was not copied")?;
    arboard::Clipboard::new()?.set_text("other")?;
    ensure!(
        clipboard.owner()? == owner,
        "The new text took another owner window"
    );
    ensure!(
        !clipboard.holds(owner, "fixture")?,
        "An owner that kept its window but changed its text still held the copy"
    );
    Ok(())
}

#[test]
#[ignore = "Takes the clipboard of a private Xvfb display it starts, never the current one"]
fn a_copy_while_the_shortcut_is_held_counts_even_from_the_same_owner_window() -> anyhow::Result<()>
{
    if !on_private_display(
        "a_copy_while_the_shortcut_is_held_counts_even_from_the_same_owner_window",
        &[],
    )? {
        return Ok(());
    }
    let (connection, owner) = selection_owner()?;
    let selection = intern(&connection, b"CLIPBOARD")?;
    let copy = || -> anyhow::Result<()> {
        connection
            .set_selection_owner(owner, selection, x11rb::CURRENT_TIME)?
            .check()?;
        Ok(())
    };
    copy()?;
    let clipboard = Clipboard::open()?;
    let ((), quiet) = clipboard.watching_owner(|_| Ok(()))?;
    ensure!(!quiet, "An untouched clipboard counted as changed");
    let ((), recopied) = clipboard.watching_owner(|_| copy())?;
    ensure!(
        recopied,
        "A new copy from the same owner window went unseen"
    );
    let ((), after) = clipboard.watching_owner(|_| Ok(()))?;
    ensure!(!after, "An earlier copy counted against a later wait");
    Ok(())
}

#[test]
#[ignore = "Takes the clipboard of a private Xvfb display without XFixes it starts, never the current one"]
fn without_xfixes_the_clipboard_opens_and_the_release_wait_compares_owner_windows()
-> anyhow::Result<()> {
    if !on_private_display(
        "without_xfixes_the_clipboard_opens_and_the_release_wait_compares_owner_windows",
        &["-extension", "XFIXES"],
    )? {
        return Ok(());
    }
    let (connection, owner) = selection_owner()?;
    ensure!(
        connection
            .extension_information(xfixes::X11_EXTENSION_NAME)?
            .is_none(),
        "The private display still offers XFixes"
    );
    let selection = intern(&connection, b"CLIPBOARD")?;
    let clipboard = Clipboard::open().context("The clipboard needed XFixes to open")?;
    let ((), quiet) = clipboard.watching_owner(|_| Ok(()))?;
    ensure!(!quiet, "An untouched clipboard counted as changed");
    let ((), copied) = clipboard.watching_owner(|_| {
        connection
            .set_selection_owner(owner, selection, x11rb::CURRENT_TIME)?
            .check()?;
        Ok(())
    })?;
    ensure!(
        copied,
        "A copy from another window went unseen without XFixes"
    );
    Ok(())
}

fn selection_owner() -> anyhow::Result<(XCBConnection, xproto::Window)> {
    let (connection, root) = connect_with_root()?;
    let window = connection.generate_id()?;
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            xproto::WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &xproto::CreateWindowAux::new(),
        )?
        .check()?;
    Ok((connection, window))
}

fn on_private_display(test: &str, server_options: &[&str]) -> anyhow::Result<bool> {
    let private = std::env::var_os(PRIVATE_DISPLAY);
    if private.is_some() && private == std::env::var_os("DISPLAY") {
        return Ok(true);
    }
    let server = PrivateServer::start(server_options)?;
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, module)| module);
    let child = Command::new(std::env::current_exe()?)
        .args(["--exact", &format!("{module}::{test}"), "--ignored"])
        .env("DISPLAY", &server.display)
        .env(PRIVATE_DISPLAY, &server.display)
        .env("XDG_SESSION_TYPE", "x11")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .output()?;
    let report = String::from_utf8_lossy(&child.stdout);
    ensure!(
        child.status.success() && report.contains("test result: ok. 1 passed"),
        "{test} failed on a private display:\n{report}"
    );
    Ok(false)
}

struct PrivateServer {
    process: Child,
    display: String,
}

impl PrivateServer {
    fn start(options: &[&str]) -> anyhow::Result<Self> {
        let first = (std::process::id() % 20_000).saturating_add(100);
        for number in (first..).take(8) {
            let display = format!(":{number}");
            let mut server = Self {
                process: Command::new("Xvfb")
                    .args([
                        display.as_str(),
                        "-displayfd",
                        "1",
                        "-nolisten",
                        "tcp",
                        "-nolisten",
                        "unix",
                    ])
                    .args(options)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .context("This check needs Xvfb")?,
                display,
            };
            let output = server.process.stdout.take().context("Xvfb has no output")?;
            let mut reported = String::new();
            BufReader::new(output).read_line(&mut reported)?;
            if reported.trim() == number.to_string() {
                return Ok(server);
            }
        }
        bail!("Xvfb found no free display")
    }
}

impl Drop for PrivateServer {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Teardown has no caller to tell; a server that already exited needs only reaping"
        )]
        let _ = self.process.kill().and_then(|()| self.process.wait());
    }
}
