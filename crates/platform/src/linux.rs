mod display_server;
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
    panic::{self, AssertUnwindSafe},
};

use anyhow::bail;
use async_channel::{Receiver, Sender};
use raw_window_handle::{RawWindowHandle, WindowHandle};
use tokio::io::{Interest, unix::AsyncFd};
use xkbcommon::xkb::Keysym;
use zbus::MatchRule;

pub use self::{
    display_server::{DisplayServer, display_server},
    tray::{NativeTray, TrayAction, TrayPresentation},
};
use super::{
    DesktopOptions, Input, InputSender, Inserted, Inserter, OwnFocus, OwnedThread,
    insertion::{Insertion, abandon_queued},
};

pub const APPLICATION_ID: &str = "io.github.krvh.speakeasy";

pub(crate) const SHOWS_UNSUPPORTED: &str = "Shows are not available on Linux yet";

const NO_PASTE_BINDING: Inserted = Inserted::Copied(
    "Text copied. This keyboard layout has no automatic paste binding; paste manually.",
);

const COPIED_FOR_MANUAL_PASTE: Inserted = Inserted::Copied("Text copied. Paste into your editor.");

const CLIPBOARD_CHANGED: Inserted = Inserted::Unavailable("The clipboard changed before pasting.");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PasteChord<K> {
    control: K,
    shift: Option<K>,
    v: K,
}

impl<K: Copy> PasteChord<K> {
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

    fn presses(self) -> impl DoubleEndedIterator<Item = K> {
        [Some(self.control), self.shift, Some(self.v)]
            .into_iter()
            .flatten()
    }
}

const DESKTOP_STOPPED: &str =
    "Linux desktop access stopped unexpectedly. Resume dictation to reconnect.";

#[derive(Debug)]
struct Actionable(&'static str);

impl fmt::Display for Actionable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Actionable {}

pub(crate) struct Monitor {
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
    own_focus: OwnFocus,
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    spawn(input, options, own_focus, serve_desktop)
}

fn spawn(
    input: InputSender,
    options: DesktopOptions,
    own_focus: OwnFocus,
    serve: impl FnOnce(
        &InputSender,
        &DesktopOptions,
        Receiver<Insertion>,
        &Receiver<Infallible>,
    ) -> anyhow::Result<()>
    + Send
    + 'static,
) -> anyhow::Result<(OwnedThread, Monitor, Inserter)> {
    let (inserter, incoming) = Inserter::lane(own_focus);
    let (stop, stopping) = async_channel::bounded(1);
    let thread = OwnedThread::spawn("desktop-access", move || {
        let lane = incoming.clone();
        let served = panic::catch_unwind(AssertUnwindSafe(|| {
            serve(&input, &options, incoming, &stopping)
        }))
        .unwrap_or_else(|_| Err(Actionable(DESKTOP_STOPPED).into()));
        match served {
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

async fn stop_requested(stopping: &Receiver<Infallible>, input: &InputSender) {
    tokio::select! {
        biased;
        _ = stopping.recv() => {},
        () = input.closed() => {},
    }
}

fn watch_readable(fd: BorrowedFd<'_>) -> anyhow::Result<AsyncFd<OwnedFd>> {
    let duplicate = fd.try_clone_to_owned()?;

    Ok(unsafe { AsyncFd::register_with_interest(duplicate, Interest::READABLE) }?)
}

fn owner_changes(name: &'static str) -> zbus::Result<MatchRule<'static>> {
    Ok(MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg(name)?
        .build())
}

pub(crate) fn check_window(handle: WindowHandle<'_>) -> anyhow::Result<()> {
    match handle.as_raw() {
        RawWindowHandle::Xcb(_) | RawWindowHandle::Xlib(_) | RawWindowHandle::Wayland(_) => Ok(()),
        _ => bail!("Linux windows require Wayland or X11"),
    }
}

pub(crate) fn reduced_motion() -> bool {
    false
}

pub(crate) fn copy_text(_: &str) -> anyhow::Result<()> {
    bail!(SHOWS_UNSUPPORTED)
}

pub(crate) fn copy_content(
    _: &crate::ClipboardContent,
    _: impl FnOnce() -> bool,
) -> anyhow::Result<bool> {
    bail!(SHOWS_UNSUPPORTED)
}

pub(crate) fn attach_stdio() -> bool {
    true
}

pub(crate) fn show_startup_error(message: &str) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "stderr is the last channel left to report a failed launch"
    )]
    let _ = writeln!(io::stderr().lock(), "speakeasy: {message}");
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

        let serve = move |_: &_, _: &_, requests: Receiver<Insertion>, _: &_| {
            let _in_hand = requests.recv_blocking()?;
            taken.try_send(())?;
            released.recv_timeout(PATIENCE)?;
            Ok(())
        };
        let (_thread, _monitor, inserter) = spawn(
            input.clone(),
            DesktopOptions::default(),
            crate::FocusReport::default().subscribe(),
            serve,
        )?;
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

    #[tokio::test]
    async fn a_panicking_desktop_thread_reports_why_and_closes_the_lane() -> anyhow::Result<()> {
        let (events, received) = async_channel::bounded(4);
        let input = InputSender::new(events);

        let serve = |_: &_, _: &_, _, _: &_| panic::resume_unwind(Box::new("private payload"));
        let (thread, _monitor, _inserter) = spawn(
            input.clone(),
            DesktopOptions::default(),
            crate::FocusReport::default().subscribe(),
            serve,
        )?;
        tokio::time::timeout(PATIENCE, thread.exited()).await?;
        match received.try_recv() {
            Ok(Input::Unavailable(reason)) => assert_eq!(reason, DESKTOP_STOPPED),
            other => anyhow::bail!("The panicked desktop thread did not report why: {other:?}"),
        }
        assert!(input.is_closed());
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
