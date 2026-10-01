//! Linux desktop resources live on an owned thread. Rendering may use Xwayland, so input
//! follows the host session rather than the presence of DISPLAY.

mod ei;
mod portal;
mod token;
mod tray;
mod x11;

use std::convert::Infallible;

use anyhow::Context;
use async_channel::{Receiver, Sender};
use raw_window_handle::RawWindowHandle;
use zbus::MatchRule;

pub use self::tray::{NativeTray, TrayAction, TrayPresentation};
use super::{Delivery, DesktopOptions, Input, InputSender, InsertPermit, Inserted, OwnedThread};

/// Desktop launcher and portal registration identity.
pub const APPLICATION_ID: &str = "io.github.krvh.speakeasy";

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

/// Sends insertion requests to the thread that owns the Linux desktop resources.
#[derive(Clone)]
pub struct Inserter {
    requests: Sender<Insertion>,
}

impl Inserter {
    /// Submits text under the recording permit through the chosen delivery.
    ///
    /// # Errors
    /// Returns native insertion failures or closure of the desktop request lane.
    pub async fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        delivery: Delivery,
    ) -> anyhow::Result<Inserted> {
        if !permit.active() {
            return Ok(Inserted::Cancelled);
        }
        let (reply, answer) = Reply::channel();
        self.requests
            .send(Insertion {
                text,
                permit,
                delivery,
                reply,
            })
            .await
            .context("Desktop input is unavailable")?;
        answer.recv().await.context("Desktop insertion stopped")?
    }
}

struct Insertion {
    text: String,
    permit: InsertPermit,
    delivery: Delivery,
    reply: Reply<anyhow::Result<Inserted>>,
}

/// The single answer to one request.
struct Reply<T>(Sender<T>);

impl<T> Reply<T> {
    fn channel() -> (Self, Receiver<T>) {
        let (sender, receiver) = async_channel::bounded(1);
        (Self(sender), receiver)
    }

    fn send(self, answer: T) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A requester that stopped waiting dropped its receiver; native work already checked its recording permit"
        )]
        let _ = self.0.try_send(answer);
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
    let (requests, incoming) = async_channel::bounded(1);
    let (stop, stop_requests) = async_channel::bounded(1);
    let thread = OwnedThread::spawn("desktop-access", move || {
        match serve_desktop(&input, &options, incoming, &stop_requests) {
            Err(error) if !input.is_closed() => input.fail(format!(
                "Linux desktop access stopped: {error}. Check desktop permissions or choose Desktop bindings/manual paste in Settings, then resume."
            )),
            _ => input.close(),
        }
    })?;
    Ok((InputMonitor { stop, thread }, Inserter { requests }))
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

/// Configures an owned UI-thread window for nonactivating, click-through presentation.
///
/// # Errors
/// Returns an error for the wrong window kind or failed native configuration.
pub fn configure_pill(handle: RawWindowHandle) -> anyhow::Result<()> {
    x11::configure_pill(handle)
}

/// Shows or hides the owned pill without activating it; call on its UI thread.
pub fn set_pill_visible(handle: RawWindowHandle, visible: bool) {
    x11::set_visible(handle, visible);
}

/// Shows or hides the owned Settings window; call on its UI thread.
pub fn set_settings_visible(handle: RawWindowHandle, visible: bool) {
    x11::set_visible(handle, visible);
}

/// Whether the desktop asks for reduced motion. Speakeasy reads no Linux desktop motion
/// preference, so the app setting decides.
#[must_use]
pub fn reduced_motion() -> bool {
    false
}

/// Presents a local startup error; callers never include audio, transcripts, or credentials.
pub fn show_error(message: &str) {
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
