//! Linux desktop input and insertion live on an owned thread, and owned windows present through
//! X11. Rendering may use Xwayland, so input follows the host session rather than the presence of
//! DISPLAY.

mod ei;
mod portal;
mod token;
mod tray;
mod x11;

use std::{
    convert::Infallible,
    fmt,
    io::{self, Write as _},
    os::fd::{BorrowedFd, OwnedFd},
};

use anyhow::bail;
use async_channel::{Receiver, Sender};
use raw_window_handle::{RawWindowHandle, WindowHandle};
use tokio::io::{Interest, unix::AsyncFd};
use xkbcommon::xkb::Keysym;
use zbus::MatchRule;

pub use self::tray::{NativeTray, TrayAction, TrayPresentation};
use super::{
    DesktopOptions, Input, InputSender, Inserted, Inserter, NativeWindow, OwnedThread,
    insertion::{Insertion, abandon_queued},
};

/// Desktop launcher and portal registration identity.
pub const APPLICATION_ID: &str = "io.github.krvh.speakeasy";
/// The failure of each Show call Linux cannot serve, since it records no Shows yet.
pub(crate) const SHOWS_UNSUPPORTED: &str = "Shows are not available on Linux yet";

/// The keyboard layout that paste types through has no paste shortcut keys: the active layout
/// through libei, or on X11 the first layout, the only one XTEST paste drives.
const NO_PASTE_BINDING: Inserted = Inserted::Copied(
    "Text copied. This keyboard layout has no automatic paste binding; paste manually.",
);
/// The user chose to paste themselves, so insertion only copies.
const COPIED_FOR_MANUAL_PASTE: Inserted = Inserted::Copied("Text copied. Paste into your editor.");
/// The X11 clipboard lost the copied text before paste.
const CLIPBOARD_CHANGED: Inserted =
    Inserted::Unavailable("Clipboard changed before paste. Dictate again.");

/// The keys of the paste shortcut: Control and V, with Shift only for terminal paste, so a layout
/// without Shift still pastes everywhere else. X11 and libei resolve it the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PasteChord<K> {
    control: K,
    shift: Option<K>,
    v: K,
}

impl<K: Copy> PasteChord<K> {
    /// Resolves the chord through `key`, which finds the key that types a symbol in the layout paste
    /// types through; none when that layout lacks a key the chord needs.
    fn resolve(terminal: bool, mut key: impl FnMut(Keysym) -> Option<K>) -> Option<Self> {
        Some(Self {
            control: key(Keysym::Control_L)?,
            shift: if terminal {
                Some(key(Keysym::Shift_L)?)
            } else {
                None
            },
            v: key(Keysym::v)?,
        })
    }

    /// The keys in press order; releases run in reverse.
    fn presses(self) -> impl DoubleEndedIterator<Item = K> {
        [Some(self.control), self.shift, Some(self.v)]
            .into_iter()
            .flatten()
    }
}

/// A desktop-access failure whose message already says what to do, so it reaches the user as is
/// rather than beside the general remedy for a failed desktop connection.
#[derive(Debug)]
struct Actionable(&'static str);

impl fmt::Display for Actionable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Actionable {}

/// What `InputMonitor` holds beside the desktop thread: the lane that asks it to stop. Insertion
/// runs on that thread or on X11 workers it owns and joins before it exits, so nothing outlives it.
pub(crate) struct Monitor {
    /// Never sends; closing it asks the desktop thread to stop.
    stop: Sender<Infallible>,
}

impl Monitor {
    pub(crate) fn request_stop(&self) {
        self.stop.close();
    }

    #[expect(
        clippy::unused_self,
        reason = "Linux insertion threads belong to the desktop thread, which joins them before exiting, so none outlives the one InputMonitor joins"
    )]
    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + Send + use<> {
        std::future::ready(())
    }
}

/// The session that owns input, whichever display server GPUI renders through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostSession {
    Wayland,
    X11,
}

impl HostSession {
    fn detect(kind: Option<&str>, wayland_display: bool) -> Self {
        match kind {
            Some("wayland") => Self::Wayland,
            Some("x11") => Self::X11,
            _ if wayland_display => Self::Wayland,
            _ => Self::X11,
        }
    }
}

/// How Settings shows the dictation and cancel shortcuts once the desktop is ready.
#[derive(Clone)]
struct ShortcutLabels {
    shortcut: String,
    cancel: String,
}

impl ShortcutLabels {
    fn new(options: &DesktopOptions) -> Self {
        if options.external_shortcut {
            Self {
                shortcut: "your desktop shortcut".into(),
                cancel: "your cancel shortcut".into(),
            }
        } else {
            Self {
                shortcut: options.shortcut.replace("LOGO", "Super"),
                cancel: options.cancel.replace("LOGO", "Super"),
            }
        }
    }

    fn into_desktop_ready(self) -> Input {
        Input::DesktopReady {
            shortcut: self.shortcut,
            cancel: self.cancel,
        }
    }
}

pub(crate) fn start(
    input: InputSender,
    options: DesktopOptions,
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    spawn(input, options, serve_desktop)
}

/// Runs `serve` on the desktop thread. Once it returns, by any path, the thread reports why and then
/// abandons every insertion still queued, since nothing reads the lane after that; the report comes
/// first, so the owner learns why before those requests fail.
fn spawn(
    input: InputSender,
    options: DesktopOptions,
    serve: impl FnOnce(
        &InputSender,
        &DesktopOptions,
        Receiver<Insertion>,
        &Receiver<Infallible>,
    ) -> anyhow::Result<()>
    + Send
    + 'static,
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    let (inserter, incoming) = Inserter::lane();
    let (stop, stopping) = async_channel::bounded(1);
    let thread = OwnedThread::spawn("desktop-access", move || {
        let lane = incoming.clone();
        match serve(&input, &options, incoming, &stopping) {
            Err(error) if !input.is_closed() => {
                input.fail(match error.downcast_ref::<Actionable>() {
                    Some(actionable) => actionable.0.to_owned(),
                    None => format!(
                        "Linux desktop access stopped: {error}. Check desktop permissions, or choose Shortcuts: Desktop bindings, Insert: Copy for manual paste, or both in Settings, then resume."
                    ),
                });
            },
            _ => input.close(),
        }
        abandon_queued(&lane);
    })?;
    Ok((thread, Monitor { stop }, inserter))
}

fn serve_desktop(
    input: &InputSender,
    options: &DesktopOptions,
    requests: Receiver<Insertion>,
    stopping: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let host = HostSession::detect(
        std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
    );
    tokio::task::LocalSet::new().block_on(&runtime, async {
        match host {
            HostSession::Wayland => portal::run(input, options, requests, stopping).await,
            HostSession::X11 => x11::run(input, options, requests, stopping).await,
        }
    })
}

/// Resolves once the owner requests a stop or input closes.
async fn stop_requested(stopping: &Receiver<Infallible>, input: &InputSender) {
    tokio::select! {
        biased;
        _ = stopping.recv() => {},
        () = input.closed() => {},
    }
}

/// Watches a duplicate of `fd` for readability on the current Tokio runtime.
fn watch_readable(fd: BorrowedFd<'_>) -> anyhow::Result<AsyncFd<OwnedFd>> {
    let duplicate = fd.try_clone_to_owned()?;
    // SAFETY: the `AsyncFd` owns the duplicate, which stays open and unchanged until it drops.
    Ok(unsafe { AsyncFd::register_with_interest(duplicate, Interest::READABLE) }?)
}

/// Matches the bus announcing that `name` gained, lost, or changed its owner.
fn owner_changes(name: &'static str) -> zbus::Result<MatchRule<'static>> {
    Ok(MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg(name)?
        .build())
}

/// An X11 or Xwayland window; each call opens its own connection to the server.
pub(crate) struct Window {
    id: u32,
}

impl Window {
    pub(crate) fn new(handle: WindowHandle<'_>) -> anyhow::Result<Self> {
        let id = match handle.as_raw() {
            RawWindowHandle::Xcb(handle) => handle.window.get(),
            RawWindowHandle::Xlib(handle) => u32::try_from(handle.window)?,
            _ => bail!("Linux windows require X11 or Xwayland"),
        };
        Ok(Self { id })
    }
}

impl NativeWindow {
    /// Configures the pill for nonactivating, click-through presentation.
    ///
    /// # Errors
    /// Fails when the X11 server cannot be reached or refuses a change to the window.
    pub fn configure_pill(&self) -> anyhow::Result<()> {
        x11::configure_pill(self.native.id)
    }

    /// Shows or hides the pill without activating it.
    pub fn set_pill_visible(&self, visible: bool) {
        x11::set_visible(self.native.id, visible);
    }

    /// Shows or hides the Settings window.
    pub fn set_settings_visible(&self, visible: bool) {
        x11::set_visible(self.native.id, visible);
    }
}

pub(crate) fn reduced_motion() -> bool {
    false
}

pub(crate) fn copy_text(_: &str) -> anyhow::Result<()> {
    bail!(SHOWS_UNSUPPORTED)
}

pub(crate) fn show_startup_error(message: &str) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "stderr is the last channel left to report a failed launch"
    )]
    let _ = writeln!(io::stderr().lock(), "Speakeasy: {message}");
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use anyhow::Context as _;
    use futures_util::FutureExt as _;

    use super::*;
    use crate::{Delivery, PATIENCE};

    #[tokio::test]
    async fn a_stopped_desktop_thread_answers_every_queued_insertion() -> anyhow::Result<()> {
        let (events, _received) = async_channel::bounded(4);
        let input = InputSender::new(events);
        let (taken, holding) = async_channel::bounded(1);
        let (release, released) = mpsc::channel();
        // Holds one request, as the portal does while it prepares a paste, until another queues.
        let serve = move |_: &_, _: &_, requests: Receiver<Insertion>, _: &_| {
            let _in_hand = requests.recv_blocking()?;
            taken.try_send(())?;
            released.recv_timeout(PATIENCE)?;
            Ok(())
        };
        let (_thread, _monitor, inserter) = spawn(input.clone(), DesktopOptions::default(), serve)?;
        let permit = input.begin().context("Recording")?;
        let in_hand = inserter.insert("fixture".into(), permit.clone(), Delivery::Paste);
        let mut in_hand = Box::pin(in_hand);
        assert!(in_hand.as_mut().now_or_never().is_none());
        tokio::time::timeout(PATIENCE, holding.recv()).await??;
        let mut queued = Box::pin(inserter.insert("fixture".into(), permit, Delivery::Paste));
        assert!(queued.as_mut().now_or_never().is_none());
        release.send(())?;
        for answer in [in_hand, queued] {
            let answer = tokio::time::timeout(PATIENCE, answer)
                .await
                .context("An insertion request was left waiting")?;
            assert!(answer.is_err());
        }
        Ok(())
    }

    #[test]
    fn paste_chords_need_shift_only_for_terminal_paste() {
        let without_shift = |symbol| (symbol != Keysym::Shift_L).then_some(symbol);
        assert_eq!(
            PasteChord::resolve(false, without_shift),
            Some(PasteChord {
                control: Keysym::Control_L,
                shift: None,
                v: Keysym::v,
            }),
            "A layout without Shift refused Ctrl+V"
        );
        assert!(PasteChord::resolve(true, without_shift).is_none());
        let terminal = PasteChord::resolve(true, Some);
        assert_eq!(
            terminal.map(|chord| chord.presses().collect::<Vec<_>>()),
            Some(vec![Keysym::Control_L, Keysym::Shift_L, Keysym::v])
        );
        assert!(
            PasteChord::resolve(false, |symbol| (symbol != Keysym::v).then_some(symbol)).is_none()
        );
    }

    #[test]
    fn host_session_is_independent_of_xwayland_rendering() {
        assert_eq!(
            HostSession::detect(Some("wayland"), false),
            HostSession::Wayland
        );
        assert_eq!(HostSession::detect(None, true), HostSession::Wayland);
        assert_eq!(HostSession::detect(Some("x11"), true), HostSession::X11);
        assert_eq!(HostSession::detect(None, false), HostSession::X11);
    }
}
