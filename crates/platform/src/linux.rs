//! Linux desktop resources live on an owned thread. Rendering may use Xwayland, so input
//! follows the host session rather than the presence of DISPLAY.

mod ei;
mod portal;
mod token;
mod tray;
mod x11;

use std::{
    convert::Infallible,
    os::fd::{BorrowedFd, OwnedFd},
};

use anyhow::bail;
use async_channel::{Receiver, Sender};
use raw_window_handle::{RawWindowHandle, WindowHandle};
use tokio::io::{Interest, unix::AsyncFd};
use zbus::MatchRule;

pub use self::tray::{NativeTray, TrayAction, TrayPresentation};
use super::{
    DesktopOptions, Input, InputSender, Inserted, Inserter, NativeWindow, OwnedThread,
    insertion::Insertion,
};

/// Desktop launcher and portal registration identity.
pub const APPLICATION_ID: &str = "io.github.krvh.speakeasy";

/// The active keyboard layout cannot type the paste shortcut, on X11 or through libei.
const NO_PASTE_BINDING: Inserted = Inserted::Copied(
    "Text copied. This keyboard layout has no automatic paste binding; paste manually.",
);
/// The user chose to paste themselves, so insertion only copies.
const COPIED_FOR_MANUAL_PASTE: Inserted = Inserted::Copied("Text copied. Paste into your editor.");

/// Owns the desktop observation thread until explicit stop and acknowledged cleanup.
pub struct InputMonitor {
    stop: Sender<Infallible>,
    thread: OwnedThread,
}

impl InputMonitor {
    /// Requests native observation shutdown without joining its thread.
    pub fn request_stop(&self) {
        self.stop.close();
    }

    /// Resolves once native resources retire; await it before replacing or dropping the monitor.
    pub fn stopped(&self) -> impl Future<Output = ()> + use<> {
        self.thread.exited()
    }
}

impl Drop for InputMonitor {
    fn drop(&mut self) {
        self.request_stop();
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

/// Starts desktop preparation; readiness arrives as `Input::DesktopReady`.
///
/// # Errors
/// Returns a desktop-thread startup failure; permission failures arrive as input events.
pub fn prepare(
    input: InputSender,
    options: DesktopOptions,
) -> anyhow::Result<(InputMonitor, Inserter)> {
    let (inserter, incoming) = Inserter::lane();
    let (stop, stop_requests) = async_channel::bounded(1);
    let thread = OwnedThread::spawn("desktop-access", move || {
        match serve_desktop(&input, &options, incoming, &stop_requests) {
            Err(error) if !input.is_closed() => input.fail(format!(
                "Linux desktop access stopped: {error}. Check desktop permissions, or choose Shortcuts: Desktop bindings, Insert: Copy for manual paste, or both in Settings, then resume."
            )),
            _ => input.close(),
        }
    })?;
    Ok((InputMonitor { stop, thread }, inserter))
}

fn serve_desktop(
    input: &InputSender,
    options: &DesktopOptions,
    requests: Receiver<Insertion>,
    stop: &Receiver<Infallible>,
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
            HostSession::Wayland => portal::run(input, options, requests, stop).await,
            HostSession::X11 => x11::run(input, options, requests, stop).await,
        }
    })
}

/// Resolves once the owner requests a stop or input closes.
async fn stopping(stop: &Receiver<Infallible>, input: &InputSender) {
    tokio::select! {
        biased;
        _ = stop.recv() => {},
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
            _ => bail!("Linux pill requires X11 or Xwayland"),
        };
        Ok(Self { id })
    }
}

impl NativeWindow {
    /// Configures the pill for nonactivating, click-through presentation.
    ///
    /// # Errors
    /// Returns failed native configuration.
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

/// Whether the desktop asks for reduced motion. Speakeasy reads no Linux desktop motion
/// preference, so the app setting decides.
#[must_use]
pub fn reduced_motion() -> bool {
    false
}

/// Places `text` on the clipboard; Linux publishes Shows in a later release.
///
/// # Errors
///
/// Always fails on Linux for now.
pub fn copy_text(_: &str) -> anyhow::Result<()> {
    anyhow::bail!("Shows are not available on Linux yet")
}

/// Writes an error that prevents or ends a launch to stderr.
///
/// The app reports runtime failures in its own windows instead. Callers never include audio,
/// transcripts, or credentials.
pub fn show_startup_error(message: &str) {
    eprintln!("Speakeasy: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

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
