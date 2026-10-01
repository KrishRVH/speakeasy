//! Wayland desktop access through xdg-desktop-portal: global shortcuts, keyboard input over libei,
//! and the portal clipboard. Every request and session the service opens is closed on retirement.

use std::{
    collections::HashMap,
    convert::Infallible,
    fs::File,
    io::Write,
    mem,
    ops::ControlFlow,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail, ensure};
use async_channel::{Receiver, Sender};
use futures_util::{FutureExt, StreamExt, future::LocalBoxFuture, stream::FuturesUnordered};
use tokio::{io::unix::AsyncFd, task::JoinSet};
use zbus::{
    Connection, MatchRule, Message, MessageStream, Proxy,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use super::{
    APPLICATION_ID, Delivery, DesktopOptions, Input, InputSender, InsertPermit, Inserted,
    Insertion, Reply, ShortcutLabels, ei, owner_changes, stopping, token::TokenStore, x11,
};
use crate::insertion::MODIFIER_WAIT;

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const DESKTOP: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
const CLIPBOARD: &str = "org.freedesktop.portal.Clipboard";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";
const PLAIN_TEXT: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];
/// Shortcut IDs Speakeasy binds; the portal reports activations by these IDs.
const DICTATE_ID: &str = "dictate";
const CANCEL_ID: &str = "cancel";
const KEYBOARD_DEVICE: u32 = 1;
const PERSIST_UNTIL_REVOKED: u32 = 2;
const PERSISTENT_REMOTE_VERSION: u32 = 2;
/// Depth of the shortcut and clipboard-ownership signal queues and of the notices derived from
/// them; it also bounds the ownership changes one clipboard handoff may see before it overruns.
const SIGNAL_BACKLOG: usize = 64;
/// Any portal restart or session closure ends desktop access, so a short queue suffices.
const REVOCATION_BACKLOG: usize = 4;
const MAX_TRANSFERS: usize = 4;
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
const SELECTION_TIMEOUT: Duration = Duration::from_secs(2);
const OWNERSHIP_SETTLE: Duration = Duration::from_millis(500);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1);

type Results = HashMap<String, OwnedValue>;
type Options = HashMap<&'static str, Value<'static>>;
type DesktopChanges = futures_util::stream::Select<MessageStream, MessageStream>;
type Transfers<'a> = FuturesUnordered<LocalBoxFuture<'a, anyhow::Result<()>>>;

struct Portal {
    connection: Connection,
    unanswered: Vec<OwnedObjectPath>,
    sessions: Vec<OwnedObjectPath>,
    nonce: String,
    sequence: u64,
}

impl Portal {
    async fn connect() -> anyhow::Result<Self> {
        let connection = Connection::session().await?;
        // Registration must be the first portal call: an earlier property read can cache an
        // empty app ID on recent desktops.
        let registered = connection
            .call_method(
                Some(DESTINATION),
                DESKTOP,
                Some("org.freedesktop.host.portal.Registry"),
                "Register",
                &(APPLICATION_ID, Options::new()),
            )
            .await;
        match registered {
            Ok(_) => {},
            // Portals without the host registry identify the app from its launcher instead.
            Err(zbus::Error::MethodError(name, _, _))
                if matches!(
                    name.as_str(),
                    "org.freedesktop.DBus.Error.UnknownMethod"
                        | "org.freedesktop.DBus.Error.UnknownInterface"
                ) => {},
            Err(error) => {
                return Err(error).context(
                    "Install the supplied Speakeasy desktop launcher before enabling portal shortcuts",
                );
            },
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(Self {
            connection,
            unanswered: Vec::new(),
            sessions: Vec::new(),
            nonce: format!("s{}_{now}", std::process::id()),
            sequence: 0,
        })
    }

    fn next_token(&mut self) -> anyhow::Result<String> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("Portal request identifiers exhausted. Pause and resume dictation.")?;
        Ok(format!("{}_{}", self.nonce, self.sequence))
    }

    /// The unique bus name as it appears in portal object paths.
    fn sender_segment(&self) -> anyhow::Result<String> {
        let name = self
            .connection
            .unique_name()
            .context("D-Bus connection has no name")?;
        Ok(name.as_str().trim_start_matches(':').replace('.', "_"))
    }

    async fn proxy(&self, interface: &'static str) -> anyhow::Result<Proxy<'static>> {
        Ok(Proxy::new(&self.connection, DESTINATION, DESKTOP, interface).await?)
    }

    async fn request<B: serde::Serialize + zbus::zvariant::DynamicType + Sync>(
        &mut self,
        interface: &'static str,
        method: &'static str,
        token: String,
        body: &B,
    ) -> anyhow::Result<Results> {
        let path: OwnedObjectPath =
            format!("{DESKTOP}/request/{}/{token}", self.sender_segment()?).try_into()?;
        let request = Proxy::new(&self.connection, DESTINATION, path.clone(), REQUEST).await?;
        let mut responses = request.receive_signal("Response").await?;
        let mut restarts = MessageStream::for_match_rule(
            owner_changes(DESTINATION)?,
            &self.connection,
            Some(REVOCATION_BACKLOG),
        )
        .await?;
        // Track the request before calling, so retirement can close a consent dialog that has
        // not answered yet.
        self.unanswered.push(path.clone());
        let returned: OwnedObjectPath = self.proxy(interface).await?.call(method, body).await?;
        ensure!(
            returned == path,
            "Portal returned an unexpected request path"
        );
        let response = tokio::select! {
            response = responses.next() => response.context("Portal request disconnected")?,
            _ = restarts.next() => bail!("The desktop portal restarted during permission setup"),
        };
        let (status, values): (u32, Results) = response.body().deserialize()?;
        self.unanswered.retain(|pending| pending != &path);
        ensure!(status == 0, "Desktop permission was declined or cancelled");
        Ok(values)
    }

    async fn create_session(&mut self, interface: &'static str) -> anyhow::Result<OwnedObjectPath> {
        let handle = self.next_token()?;
        let session = self.next_token()?;
        let predicted: OwnedObjectPath =
            format!("{DESKTOP}/session/{}/{session}", self.sender_segment()?).try_into()?;
        self.sessions.push(predicted.clone());
        let options = Options::from([
            ("handle_token", Value::from(handle.clone())),
            ("session_handle_token", Value::from(session)),
        ]);
        let mut result = self
            .request(interface, "CreateSession", handle, &(options,))
            .await?;
        let actual = String::try_from(
            result
                .remove("session_handle")
                .context("Portal did not create a session")?,
        )?;
        ensure!(
            actual == predicted.as_str(),
            "Portal returned an unexpected session path"
        );
        Ok(predicted)
    }

    async fn close(&self) {
        for (paths, interface) in [(&self.unanswered, REQUEST), (&self.sessions, SESSION)] {
            for path in paths {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Closing retired portal objects is bounded best effort when the desktop service has disconnected"
                )]
                let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
                    Proxy::new(&self.connection, DESTINATION, path.clone(), interface)
                        .await?
                        .call::<_, _, ()>("Close", &())
                        .await
                })
                .await;
            }
        }
    }
}

/// What the portal granted: a shortcut session and a remote desktop for keyboard input.
struct Bound {
    shortcut_session: Option<OwnedObjectPath>,
    remote: Option<Remote>,
    labels: ShortcutLabels,
}

impl Bound {
    fn sender(&self) -> Option<&ei::Sender> {
        self.remote.as_ref().map(|remote| &remote.sender)
    }

    fn sender_mut(&mut self) -> Option<&mut ei::Sender> {
        self.remote.as_mut().map(|remote| &mut remote.sender)
    }

    fn clipboard_session(&self) -> Option<&OwnedObjectPath> {
        self.remote
            .as_ref()
            .filter(|remote| remote.shares_clipboard)
            .map(|remote| &remote.session)
    }

    fn dispatch(&mut self) -> anyhow::Result<()> {
        if let Some(sender) = self.sender_mut() {
            sender.dispatch()?;
        }
        Ok(())
    }
}

struct Remote {
    session: OwnedObjectPath,
    sender: ei::Sender,
    shares_clipboard: bool,
}

/// The insertion pipeline: requests wait for released modifiers, then copy, confirm clipboard
/// ownership, and check focus before submitting native input.
struct Insertions<'a> {
    transfers: Transfers<'a>,
    preparing: Option<Preparing<'a>>,
    pending: Option<(Insertion, Instant)>,
    selection: Option<Selection>,
    bound: &'a mut Bound,
    notices: &'a Receiver<Notice>,
    selecting: &'a Sender<SelectionHandoff>,
    worker: &'a x11::ClipboardWorker,
    clipboard: &'a Proxy<'static>,
    options: &'a DesktopOptions,
}

impl<'a> Insertions<'a> {
    fn is_idle(&self) -> bool {
        self.pending.is_none() && self.preparing.is_none()
    }

    fn accepts_transfers(&self) -> bool {
        self.bound.clipboard_session().is_some()
            && self.transfers.len() < MAX_TRANSFERS
            && !self
                .preparing
                .as_ref()
                .is_some_and(|work| work.setting_selection)
    }

    fn apply_queued_notices(&mut self) {
        while let Ok(notice) = self.notices.try_recv() {
            self.apply(notice);
        }
    }

    fn apply(&mut self, notice: Notice) {
        apply_notice(notice, self.bound.sender_mut(), &mut self.selection);
    }

    fn queue(&mut self, request: Insertion) {
        self.pending = Some((request, Instant::now()));
    }

    /// Catches up on notices and libei events, then starts the pending request once its wait for
    /// released modifiers is over.
    fn advance_pending(&mut self) -> anyhow::Result<()> {
        self.apply_queued_notices();
        self.bound.dispatch()?;
        let Some((request, _)) = self.pending.take_if(|(request, started)| {
            !request.permit.active()
                || self.options.manual_paste
                || request.delivery == Delivery::Direct
                || self.bound.sender().is_some_and(ei::Sender::can_paste)
                || started.elapsed() >= MODIFIER_WAIT
        }) else {
            return Ok(());
        };
        if !request.permit.active() {
            request.reply.send(Ok(Inserted::Cancelled));
            return Ok(());
        }
        let (task, setting_selection) = match request.delivery {
            Delivery::Direct => (
                query_focus(self.worker, request.permit.clone(), Route::Direct),
                false,
            ),
            Delivery::Paste => (
                self.copy(&request),
                self.bound.clipboard_session().is_some(),
            ),
        };
        self.preparing = Some(Preparing {
            request,
            setting_selection,
            task,
        });
        Ok(())
    }

    fn copy(&self, request: &Insertion) -> LocalBoxFuture<'a, anyhow::Result<Prepared>> {
        let permit = request.permit.clone();
        match self.bound.clipboard_session() {
            Some(session) => set_selection(self.clipboard, session.clone(), permit),
            None => copy_with_x11(self.worker, request.text.clone(), permit),
        }
    }

    fn advance_preparation(&mut self, prepared: anyhow::Result<Prepared>) -> anyhow::Result<()> {
        let request = self
            .preparing
            .take()
            .context("Missing clipboard preparation")?
            .request;
        let permit = request.permit.clone();
        let task = match prepared {
            Ok(Prepared::SelectionSet) => {
                self.selection = Some(Selection::new(&request.text, permit.clone()));
                confirm_selection(self.selecting, permit)
            },
            Ok(Prepared::Copied { owner: Some(owner) }) => {
                confirm_owner(self.worker, owner, permit)
            },
            Ok(Prepared::Ownership { owned }) => {
                query_focus(self.worker, permit, Route::Paste { owned })
            },
            Ok(Prepared::Copied { owner: None }) => {
                let outcome = if permit.active() {
                    Inserted::Unavailable("Clipboard changed before paste. Dictate again.")
                } else {
                    Inserted::Cancelled
                };
                request.reply.send(Ok(outcome));
                return Ok(());
            },
            Ok(Prepared::Target { external, route }) => {
                self.finish(request, external, route);
                return Ok(());
            },
            Err(error) => {
                request.reply.send(Err(error));
                return Ok(());
            },
        };
        self.preparing = Some(Preparing {
            request,
            setting_selection: false,
            task,
        });
        Ok(())
    }

    fn finish(&mut self, request: Insertion, external: bool, route: Route) {
        // Notices queued while preparation awaited other processes must apply first: a missed
        // press could paste while the shortcut is held.
        self.apply_queued_notices();
        let outcome = match route {
            Route::Direct => insert_direct(self.bound, &request, external),
            Route::Paste { owned } => {
                if !owned && self.bound.clipboard_session().is_some() {
                    self.selection = None;
                }
                finish_paste(self.bound, &request, self.options, owned, external)
            },
        };
        request.reply.send(outcome);
    }

    fn serve_transfer(&self, event: &Message) -> anyhow::Result<()> {
        let (path, mime, serial): (OwnedObjectPath, String, u32) = event.body().deserialize()?;
        if self.bound.clipboard_session() != Some(&path) {
            return Ok(());
        }
        let payload = self
            .selection
            .as_ref()
            .filter(|_| PLAIN_TEXT.contains(&mime.as_str()))
            .map(Selection::transfer);
        let clipboard = self.clipboard;
        let session = path.clone();
        let write = async move {
            let fd: zbus::zvariant::OwnedFd =
                clipboard.call("SelectionWrite", &(session, serial)).await?;
            Ok(OwnedFd::from(fd))
        };
        let done = move |success: bool| async move {
            clipboard
                .call::<_, _, ()>("SelectionWriteDone", &(path, serial, success))
                .await?;
            Ok(())
        };
        self.transfers
            .push(transfer(write, done, payload).boxed_local());
        Ok(())
    }

    fn cancel_all(self) {
        if let Some((request, _)) = self.pending {
            request.reply.send(Ok(Inserted::Cancelled));
        }
        if let Some(work) = self.preparing {
            work.request.reply.send(Ok(Inserted::Cancelled));
        }
    }
}

struct Preparing<'a> {
    request: Insertion,
    /// Whether `task` is `SetSelection`. Transfers wait until it returns, because only then does
    /// `selection` hold the new text.
    setting_selection: bool,
    task: LocalBoxFuture<'a, anyhow::Result<Prepared>>,
}

/// The outcome of one preparation stage.
enum Prepared {
    /// The portal clipboard offers the text; this session's ownership awaits confirmation.
    SelectionSet,
    /// The X11 clipboard holds the text under the `owner` window, or `None` when the clipboard
    /// changed or the permit lapsed first.
    Copied { owner: Option<u32> },
    /// Whether this session still owns the clipboard selection it set.
    Ownership { owned: bool },
    /// Whether another app has focus, and how the insertion submits.
    Target { external: bool, route: Route },
}

/// How a prepared insertion submits once focus is known.
enum Route {
    Direct,
    Paste { owned: bool },
}

enum Notice {
    ModifiersChanged,
    SelectionLost(InsertPermit),
}

/// The text a portal clipboard selection serves until a newer selection replaces it.
struct Selection {
    text: Arc<str>,
    permit: InsertPermit,
    invalidate: Sender<Infallible>,
    invalidated: Receiver<Infallible>,
}

impl Selection {
    fn new(text: &str, permit: InsertPermit) -> Self {
        let (invalidate, invalidated) = async_channel::bounded(1);
        Self {
            text: text.into(),
            permit,
            invalidate,
            invalidated,
        }
    }

    fn transfer(&self) -> TransferPayload {
        TransferPayload {
            text: self.text.clone(),
            invalidated: self.invalidated.clone(),
        }
    }
}

impl Drop for Selection {
    fn drop(&mut self) {
        self.invalidate.close();
    }
}

struct TransferPayload {
    text: Arc<str>,
    invalidated: Receiver<Infallible>,
}

/// Watches shortcuts, session revocation, and clipboard ownership while insertion awaits other
/// processes, revoking a permit as soon as its selection is lost.
struct Pump {
    input: InputSender,
    shortcut_session: Option<OwnedObjectPath>,
    sessions: Vec<OwnedObjectPath>,
    shortcuts: MessageStream,
    changes: DesktopChanges,
    owners: MessageStream,
    selections: Receiver<SelectionHandoff>,
    notify: Sender<Notice>,
    ownership: SelectionOwnership,
    held: bool,
    handoff: Option<PendingHandoff>,
}

/// A selection handoff waiting for its ownership signals to settle.
struct PendingHandoff {
    handoff: SelectionHandoff,
    started: Instant,
    events: usize,
}

impl Pump {
    async fn watch(self) {
        let input = self.input.clone();
        if let Err(error) = self.run().await {
            input.fail(format!(
                "Desktop access stopped: {error}. Resume dictation to reconnect."
            ));
        }
    }

    async fn run(mut self) -> anyhow::Result<()> {
        loop {
            self.complete_settled_handoff()?;
            let settle = or_pending(
                self.handoff
                    .as_ref()
                    .map(|pending| deadline(pending.started, OWNERSHIP_SETTLE)),
            );
            tokio::select! {
                biased;
                () = self.input.closed() => return Ok(()),
                handoff = self.selections.recv(), if self.handoff.is_none() => {
                    self.begin_handoff(handoff.context("Clipboard preparation stopped")?)?;
                },
                event = self.owners.next(), if self.ownership.session.is_some() => {
                    self.observe_owner(&event.context("Clipboard portal disconnected")??)?;
                },
                event = self.changes.next() => {
                    self.check_session(&event.context("Portal session monitoring stopped")??)?;
                },
                event = self.shortcuts.next(), if self.shortcut_session.is_some() => {
                    let event = event.context("Shortcut portal disconnected")??;
                    if self.on_shortcut(&event)?.is_break() {
                        return Ok(());
                    }
                },
                () = settle => {},
            }
        }
    }

    fn complete_settled_handoff(&mut self) -> anyhow::Result<()> {
        let owned = self.ownership.owned;
        let Some(PendingHandoff { handoff, .. }) = self
            .handoff
            .take_if(|pending| owned || pending.started.elapsed() >= OWNERSHIP_SETTLE)
        else {
            return Ok(());
        };
        self.ownership.adopt(handoff.permit, &self.notify)?;
        handoff.ready.send(owned);
        Ok(())
    }

    fn begin_handoff(&mut self, handoff: SelectionHandoff) -> anyhow::Result<()> {
        // SetSelection has completed: its queued ownership signals still belong to the
        // preceding permit, so settle them before the new selection takes over.
        let mut drained = 0;
        while drained < SIGNAL_BACKLOG
            && let Some(event) = self.owners.next().now_or_never()
        {
            self.ownership.observe(
                &event.context("Clipboard portal disconnected")??,
                &self.notify,
            )?;
            drained = drained.saturating_add(1);
        }
        ensure!(
            drained < SIGNAL_BACKLOG || self.owners.next().now_or_never().is_none(),
            "Clipboard ownership queue overran"
        );
        self.handoff = Some(PendingHandoff {
            handoff,
            started: Instant::now(),
            events: 0,
        });
        Ok(())
    }

    fn observe_owner(&mut self, event: &Message) -> anyhow::Result<()> {
        self.ownership.observe(event, &self.notify)?;
        if let Some(pending) = &mut self.handoff {
            pending.events = pending.events.saturating_add(1);
            ensure!(
                pending.events <= SIGNAL_BACKLOG,
                "Clipboard ownership queue overran"
            );
        }
        Ok(())
    }

    fn check_session(&self, event: &Message) -> anyhow::Result<()> {
        let header = event.header();
        if header
            .member()
            .is_some_and(|member| member.as_str() == "NameOwnerChanged")
        {
            bail!("The desktop portal restarted");
        }
        if let Some(closed) = header.path()
            && self
                .sessions
                .iter()
                .any(|session| session.as_str() == closed.as_str())
        {
            bail!("Desktop permissions were revoked");
        }
        Ok(())
    }

    fn on_shortcut(&mut self, event: &Message) -> anyhow::Result<ControlFlow<()>> {
        let header = event.header();
        let Some(member) = header
            .member()
            .filter(|member| matches!(member.as_str(), "Activated" | "Deactivated"))
        else {
            return Ok(ControlFlow::Continue(()));
        };
        let (path, action, _, _): (OwnedObjectPath, String, u64, Results) =
            event.body().deserialize()?;
        if Some(&path) != self.shortcut_session.as_ref() {
            return Ok(ControlFlow::Continue(()));
        }
        let activated = member.as_str() == "Activated";
        let Some(input) = shortcut_event(&mut self.held, &action, activated) else {
            return Ok(ControlFlow::Continue(()));
        };
        // An insertion that misses this press could paste while the shortcut is still held.
        if matches!(input, Input::Press) && self.notify.try_send(Notice::ModifiersChanged).is_err()
        {
            self.input.close();
            return Ok(ControlFlow::Break(()));
        }
        self.input.deliver(input);
        Ok(ControlFlow::Continue(()))
    }
}

/// A new portal selection awaiting confirmation that this session owns it.
struct SelectionHandoff {
    permit: InsertPermit,
    ready: Reply<bool>,
}

/// Which recording's portal selection this session currently owns.
struct SelectionOwnership {
    session: Option<OwnedObjectPath>,
    monitored: Option<InsertPermit>,
    owned: bool,
}

impl SelectionOwnership {
    fn observe(&mut self, event: &Message, notify: &Sender<Notice>) -> anyhow::Result<()> {
        let (path, values): (OwnedObjectPath, Results) = event.body().deserialize()?;
        if Some(&path) != self.session.as_ref() {
            return Ok(());
        }
        self.owned = values
            .get("session_is_owner")
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or(false);
        if !self.owned
            && let Some(permit) = self.monitored.take()
        {
            lose_selection(permit, notify)?;
        }
        Ok(())
    }

    fn adopt(&mut self, permit: InsertPermit, notify: &Sender<Notice>) -> anyhow::Result<()> {
        if self.owned {
            self.monitored = Some(permit);
            Ok(())
        } else {
            lose_selection(permit, notify)
        }
    }
}

#[expect(
    clippy::future_not_send,
    reason = "libei resources remain on the owned desktop thread and its LocalSet; native pointers must never move between threads"
)]
pub(super) async fn run(
    input: &InputSender,
    options: &DesktopOptions,
    requests: Receiver<Insertion>,
    stop: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    // Desktop bindings and manual paste need neither portal shortcuts nor keyboard input;
    // Xwayland serves the copy.
    if options.manual_paste && options.external_shortcut {
        return x11::run(input, options, requests, stop).await;
    }
    let mut portal = tokio::select! {
        biased;
        () = stopping(stop, input) => return Ok(()),
        portal = Portal::connect() => portal?,
    };
    let bound = tokio::select! {
        biased;
        () = stopping(stop, input) => None,
        bound = bind(&mut portal, options) => Some(bound),
    };
    let outcome = match bound {
        Some(Ok(bound)) => serve(&portal, bound, input, options, requests, stop).await,
        Some(Err(error)) => Err(error),
        None => Ok(()),
    };
    portal.close().await;
    outcome
}

async fn bind(portal: &mut Portal, options: &DesktopOptions) -> anyhow::Result<Bound> {
    let mut labels = ShortcutLabels::new(options);
    let shortcut_session = if options.external_shortcut {
        None
    } else {
        Some(bind_shortcuts(portal, options, &mut labels).await?)
    };
    let remote = if options.manual_paste {
        None
    } else {
        Some(start_remote_desktop(portal).await?)
    };
    Ok(Bound {
        shortcut_session,
        remote,
        labels,
    })
}

async fn bind_shortcuts(
    portal: &mut Portal,
    options: &DesktopOptions,
    labels: &mut ShortcutLabels,
) -> anyhow::Result<OwnedObjectPath> {
    let session = portal.create_session(SHORTCUTS).await?;
    let handle = portal.next_token()?;
    let shortcuts = vec![
        (
            DICTATE_ID,
            Options::from([
                ("description", Value::from("Hold to dictate")),
                ("preferred_trigger", Value::from(options.shortcut.clone())),
            ]),
        ),
        (
            CANCEL_ID,
            Options::from([
                ("description", Value::from("Cancel dictation")),
                ("preferred_trigger", Value::from(options.cancel.clone())),
            ]),
        ),
    ];
    let request = Options::from([("handle_token", Value::from(handle.clone()))]);
    let mut result = portal
        .request(
            SHORTCUTS,
            "BindShortcuts",
            handle,
            &(session.clone(), shortcuts, "", request),
        )
        .await?;
    let bindings: Vec<(String, Results)> = result
        .remove("shortcuts")
        .context("No shortcut bindings were granted")?
        .try_into()?;
    ensure!(
        bindings.iter().any(|(id, _)| id == DICTATE_ID)
            && bindings.iter().any(|(id, _)| id == CANCEL_ID),
        "Both dictate and cancel shortcuts must be enabled"
    );
    for (id, mut values) in bindings {
        if let Some(trigger) = values.remove("trigger_description") {
            match id.as_str() {
                DICTATE_ID => labels.shortcut = trigger.try_into()?,
                CANCEL_ID => labels.cancel = trigger.try_into()?,
                _ => {},
            }
        }
    }
    Ok(session)
}

async fn start_remote_desktop(portal: &mut Portal) -> anyhow::Result<Remote> {
    let tokens = TokenStore::open();
    let session = portal.create_session(REMOTE).await?;
    let handle = portal.next_token()?;
    let mut devices = Options::from([
        ("handle_token", Value::from(handle.clone())),
        ("types", Value::from(KEYBOARD_DEVICE)),
    ]);
    if let Some(store) = &tokens
        && portal
            .proxy(REMOTE)
            .await?
            .get_property::<u32>("version")
            .await?
            >= PERSISTENT_REMOTE_VERSION
    {
        devices.insert("persist_mode", Value::from(PERSIST_UNTIL_REVOKED));
        if let Ok(Some(previous)) = store.consume() {
            devices.insert("restore_token", Value::from(previous));
        }
    }
    portal
        .request(REMOTE, "SelectDevices", handle, &(session.clone(), devices))
        .await?;
    let clipboard_requested = request_clipboard(portal, &session).await?;
    let handle = portal.next_token()?;
    let start = Options::from([("handle_token", Value::from(handle.clone()))]);
    let mut result = portal
        .request(REMOTE, "Start", handle, &(session.clone(), "", start))
        .await?;
    let granted: u32 = result
        .remove("devices")
        .context("Keyboard permission was not granted")?
        .try_into()?;
    ensure!(
        granted & KEYBOARD_DEVICE != 0,
        "Keyboard permission was not granted"
    );
    if let Some(store) = tokens
        && let Some(token) = result
            .remove("restore_token")
            .and_then(|value| String::try_from(value).ok())
    {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Optional restore-token persistence must not discard keyboard permission the user just granted"
        )]
        let _ = store.save(&token);
    }
    let shares_clipboard = clipboard_requested
        && result
            .remove("clipboard_enabled")
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or(false);
    let descriptor: zbus::zvariant::OwnedFd = portal
        .proxy(REMOTE)
        .await?
        .call("ConnectToEIS", &(session.clone(), Options::new()))
        .await?;
    Ok(Remote {
        sender: ei::Sender::connect(OwnedFd::from(descriptor))?,
        session,
        shares_clipboard,
    })
}

async fn request_clipboard(portal: &Portal, session: &OwnedObjectPath) -> anyhow::Result<bool> {
    let Ok(clipboard) = portal.proxy(CLIPBOARD).await else {
        return Ok(false);
    };
    // Creating a proxy does not contact the portal; reading `version` proves the interface exists.
    if clipboard.get_property::<u32>("version").await.is_err() {
        return Ok(false);
    }
    clipboard
        .call::<_, _, ()>("RequestClipboard", &(session.clone(), Options::new()))
        .await?;
    Ok(true)
}

#[expect(
    clippy::future_not_send,
    reason = "libei resources remain on the owned desktop thread and its LocalSet; native pointers must never move between threads"
)]
async fn serve(
    portal: &Portal,
    mut bound: Bound,
    input: &InputSender,
    options: &DesktopOptions,
    requests: Receiver<Insertion>,
    stop: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    let shortcuts = shortcut_signals(&portal.connection).await?;
    let clipboard = portal.proxy(CLIPBOARD).await?;
    let mut transfer_requests = clipboard.receive_signal("SelectionTransfer").await?;
    let owners = selection_owner_changes(&portal.connection).await?;
    let changes = desktop_changes(&portal.connection).await?;
    let (notify, notices) = async_channel::bounded(SIGNAL_BACKLOG);
    let (selecting, selections) = async_channel::bounded(1);
    // Dropping the set aborts the pump once serving ends.
    let mut pump = JoinSet::new();
    pump.spawn_local(
        Pump {
            input: input.clone(),
            shortcut_session: bound.shortcut_session.clone(),
            sessions: portal.sessions.clone(),
            shortcuts,
            changes,
            owners,
            selections,
            notify,
            ownership: SelectionOwnership {
                session: bound.clipboard_session().cloned(),
                monitored: None,
                owned: false,
            },
            held: false,
            handoff: None,
        }
        .watch(),
    );
    // Focus checks need the X worker, which opens the X clipboard only for X11 copies; portal
    // clipboard and direct text never touch clipboard contents.
    let worker = x11::ClipboardWorker::spawn(input.clone())?;
    tokio::select! {
        biased;
        () = stopping(stop, input) => return Ok(()),
        ready = worker.ready() => ready?,
    }
    input.deliver(bound.labels.clone().into_desktop_ready());
    let mut insertions = Insertions {
        transfers: Transfers::new(),
        preparing: None,
        pending: None,
        selection: None,
        bound: &mut bound,
        notices: &notices,
        selecting: &selecting,
        worker: &worker,
        clipboard: &clipboard,
        options,
    };
    loop {
        insertions.advance_pending()?;
        let accepts_transfers = insertions.accepts_transfers();
        let idle = insertions.is_idle();
        let prepared = or_pending(insertions.preparing.as_mut().map(|work| work.task.as_mut()));
        let readable = or_pending(
            insertions
                .bound
                .sender()
                .map(|sender| sender.readiness().readable()),
        );
        let modifier_wait = or_pending(
            insertions
                .pending
                .as_ref()
                .map(|(_, started)| deadline(*started, MODIFIER_WAIT)),
        );
        tokio::select! {
            biased;
            () = stopping(stop, input) => break,
            notice = notices.recv() => {
                insertions.apply(notice.context("Desktop monitoring stopped")?);
            },
            prepared = prepared => insertions.advance_preparation(prepared)?,
            written = insertions.transfers.next(), if !insertions.transfers.is_empty() => {
                written.context("Clipboard transfer stopped")??;
            },
            event = transfer_requests.next(), if accepts_transfers => {
                insertions.serve_transfer(&event.context("Clipboard portal disconnected")?)?;
            },
            request = requests.recv(), if idle => match request {
                Ok(request) => insertions.queue(request),
                Err(_) => break,
            },
            readable = readable => readable?.clear_ready(),
            () = modifier_wait => {},
        }
    }
    insertions.cancel_all();
    Ok(())
}

/// Activation and release share one stream: separate queues can reorder a double-tap into two
/// activations followed by two releases.
async fn shortcut_signals(connection: &Connection) -> anyhow::Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DESTINATION)?
        .interface(SHORTCUTS)?
        .path(DESKTOP)?
        .build();
    Ok(MessageStream::for_match_rule(rule, connection, Some(SIGNAL_BACKLOG)).await?)
}

/// Clipboard ownership changes, queued to the depth a handoff drains.
async fn selection_owner_changes(connection: &Connection) -> anyhow::Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DESTINATION)?
        .interface(CLIPBOARD)?
        .member("SelectionOwnerChanged")?
        .path(DESKTOP)?
        .build();
    Ok(MessageStream::for_match_rule(rule, connection, Some(SIGNAL_BACKLOG)).await?)
}

/// Session closures and portal restarts, either of which revokes desktop access.
async fn desktop_changes(connection: &Connection) -> anyhow::Result<DesktopChanges> {
    let closed = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DESTINATION)?
        .interface(SESSION)?
        .member("Closed")?
        .build();
    Ok(futures_util::stream::select(
        MessageStream::for_match_rule(closed, connection, Some(REVOCATION_BACKLOG)).await?,
        MessageStream::for_match_rule(
            owner_changes(DESTINATION)?,
            connection,
            Some(REVOCATION_BACKLOG),
        )
        .await?,
    ))
}

/// Pends unless a future is present; resolves once it completes.
async fn or_pending<F: Future>(future: Option<F>) -> F::Output {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

fn deadline(started: Instant, wait: Duration) -> tokio::time::Sleep {
    tokio::time::sleep_until(started.checked_add(wait).unwrap_or(started).into())
}

fn set_selection<'a>(
    clipboard: &'a Proxy<'static>,
    session: OwnedObjectPath,
    permit: InsertPermit,
) -> LocalBoxFuture<'a, anyhow::Result<Prepared>> {
    async move {
        if !permit.active() {
            return Ok(Prepared::Copied { owner: None });
        }
        let mime_types = Options::from([("mime_types", Value::from(PLAIN_TEXT.to_vec()))]);
        tokio::time::timeout(
            SELECTION_TIMEOUT,
            clipboard.call::<_, _, ()>("SetSelection", &(session, mime_types)),
        )
        .await??;
        Ok(Prepared::SelectionSet)
    }
    .boxed_local()
}

fn copy_with_x11(
    worker: &x11::ClipboardWorker,
    text: String,
    permit: InsertPermit,
) -> LocalBoxFuture<'_, anyhow::Result<Prepared>> {
    async move {
        Ok(Prepared::Copied {
            owner: worker.set_text(text, permit).await?,
        })
    }
    .boxed_local()
}

fn confirm_selection(
    selecting: &Sender<SelectionHandoff>,
    permit: InsertPermit,
) -> LocalBoxFuture<'_, anyhow::Result<Prepared>> {
    async move {
        let (ready, confirmed) = Reply::channel();
        selecting.send(SelectionHandoff { permit, ready }).await?;
        let owned = confirmed
            .recv()
            .await
            .context("Clipboard ownership monitoring stopped")?;
        Ok(Prepared::Ownership { owned })
    }
    .boxed_local()
}

fn confirm_owner(
    worker: &x11::ClipboardWorker,
    owner: u32,
    permit: InsertPermit,
) -> LocalBoxFuture<'_, anyhow::Result<Prepared>> {
    async move {
        Ok(Prepared::Ownership {
            owned: worker.owns(owner, permit).await?,
        })
    }
    .boxed_local()
}

fn query_focus(
    worker: &x11::ClipboardWorker,
    permit: InsertPermit,
    route: Route,
) -> LocalBoxFuture<'_, anyhow::Result<Prepared>> {
    async move {
        // A failed focus query fails closed, as if Speakeasy had focus.
        let external = worker.external_target(permit).await.unwrap_or(false);
        Ok(Prepared::Target { external, route })
    }
    .boxed_local()
}

fn insert_direct(
    bound: &mut Bound,
    request: &Insertion,
    external: bool,
) -> anyhow::Result<Inserted> {
    if !request.permit.active() {
        return Ok(Inserted::Cancelled);
    }
    if let Some(outcome) = Delivery::Direct.check_focus(external) {
        return Ok(outcome);
    }
    if request.text.contains('\0') {
        return Ok(Inserted::Unavailable(
            "Direct input cannot contain a null character. Turn off Keep clipboard to use paste.",
        ));
    }
    bound.dispatch()?;
    let Some(sender) = bound.sender_mut().filter(|sender| sender.can_type_text()) else {
        return Ok(Inserted::Unavailable(
            "This desktop does not support direct text input. Turn off Keep clipboard to use paste.",
        ));
    };
    if !request.permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    if sender.type_text(&request.text).is_err() {
        return Ok(Inserted::Unavailable(
            "Direct text input could not complete. Check your editor before dictating again; submission was not repeated.",
        ));
    }
    Ok(Inserted::Sent)
}

fn finish_paste(
    bound: &mut Bound,
    request: &Insertion,
    options: &DesktopOptions,
    owned: bool,
    external: bool,
) -> anyhow::Result<Inserted> {
    bound.dispatch()?;
    if !request.permit.active() {
        return Ok(Inserted::Cancelled);
    }
    if !owned {
        return Ok(Inserted::Unavailable(
            if bound.clipboard_session().is_some() {
                "Clipboard ownership could not be confirmed. Dictate again or choose Copy for manual paste."
            } else {
                "Clipboard changed before paste. Dictate again."
            },
        ));
    }
    if let Some(outcome) = Delivery::Paste.check_focus(external) {
        return Ok(outcome);
    }
    if options.manual_paste {
        return Ok(Inserted::Copied(
            "Text copied. Release your shortcut keys and paste into your editor.",
        ));
    }
    let Some(sender) = bound.sender_mut().filter(|sender| sender.can_paste()) else {
        return Ok(Inserted::Copied(
            "Text copied. This desktop could not confirm released shortcut keys; paste manually.",
        ));
    };
    if !request.permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    if sender.paste(options.terminal_paste).is_err() {
        return Ok(Inserted::Copied(
            "Text copied, but input submission failed. Paste manually; automatic paste was not repeated.",
        ));
    }
    Ok(Inserted::Sent)
}

fn apply_notice(
    notice: Notice,
    sender: Option<&mut ei::Sender>,
    selection: &mut Option<Selection>,
) {
    match notice {
        Notice::ModifiersChanged => {
            if let Some(sender) = sender {
                sender.forget_modifiers();
            }
        },
        Notice::SelectionLost(permit) => {
            if selection
                .as_ref()
                .is_some_and(|selection| selection.permit.same_recording(&permit))
            {
                *selection = None;
            }
        },
    }
}

async fn transfer<D: Future<Output = anyhow::Result<()>>>(
    write: impl Future<Output = anyhow::Result<OwnedFd>>,
    done: impl FnOnce(bool) -> D,
    payload: Option<TransferPayload>,
) -> anyhow::Result<()> {
    // Editors may request the payload after paste commits, so only selection loss ends serving.
    let success = match payload {
        Some(payload) => tokio::select! {
            biased;
            _ = payload.invalidated.recv() => false,
            written = write_payload(write, &payload.text) => written.is_ok(),
        },
        None => false,
    };
    tokio::time::timeout(TRANSFER_TIMEOUT, done(success)).await??;
    Ok(())
}

async fn write_payload(
    write: impl Future<Output = anyhow::Result<OwnedFd>>,
    text: &str,
) -> anyhow::Result<()> {
    let fd = tokio::time::timeout(TRANSFER_TIMEOUT, write).await??;
    set_nonblocking(fd.as_fd())?;
    let pipe = AsyncFd::new(File::from(fd))?;
    tokio::time::timeout(TRANSFER_TIMEOUT, write_to_pipe(&pipe, text.as_bytes())).await?
}

async fn write_to_pipe(pipe: &AsyncFd<File>, mut bytes: &[u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        let mut writable = pipe.writable().await?;
        if let Ok(written) = writable.try_io(|pipe| pipe.get_ref().write(bytes)) {
            let written = written?;
            ensure!(written > 0, "Clipboard transfer stopped");
            bytes = bytes
                .get(written..)
                .context("Clipboard write exceeded its pending transfer")?;
        }
    }
    Ok(())
}

fn set_nonblocking(fd: BorrowedFd<'_>) -> anyhow::Result<()> {
    let fd = fd.as_raw_fd();
    // SAFETY: the borrowed descriptor stays open; F_GETFL and F_SETFL only read and change its
    // status flags.
    let updated = unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
    };
    ensure!(updated, "Cannot prepare clipboard transfer");
    Ok(())
}

fn lose_selection(permit: InsertPermit, notify: &Sender<Notice>) -> anyhow::Result<()> {
    permit.revoke();
    notify.try_send(Notice::SelectionLost(permit))?;
    Ok(())
}

fn shortcut_event(held: &mut bool, action: &str, activated: bool) -> Option<Input> {
    match (action, activated) {
        (DICTATE_ID, true) if !*held => {
            *held = true;
            Some(Input::Press)
        },
        (DICTATE_ID, false) if mem::take(held) => Some(Input::Release),
        (CANCEL_ID, true) => Some(Input::Cancel),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Read as _, os::unix::net::UnixStream};

    use super::*;

    #[test]
    fn cancelled_direct_input_precedes_focus_and_capability_errors() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let permit = input.begin().context("Recording")?;
        permit.revoke();
        let mut bound = Bound {
            shortcut_session: None,
            remote: None,
            labels: ShortcutLabels::new(&DesktopOptions::default()),
        };
        for external in [false, true] {
            for text in ["fixture", "\0"] {
                let (reply, _answer) = Reply::channel();
                let request = Insertion {
                    text: text.into(),
                    permit: permit.clone(),
                    delivery: Delivery::Direct,
                    reply,
                };
                assert!(matches!(
                    insert_direct(&mut bound, &request, external)?,
                    Inserted::Cancelled
                ));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn stalled_transfers_allow_new_work_and_retire_without_detached_writes()
    -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let permit = input.begin().context("Recording")?;
        let selection = Selection::new(&"fixture".repeat(128 * 1024), permit);
        let (writer, mut reader) = UnixStream::pair()?;
        reader.set_read_timeout(Some(Duration::from_millis(100)))?;
        let (entered, opened) = async_channel::bounded(1);
        let mut transfers = Transfers::new();
        transfers.push(
            transfer(
                async move {
                    entered.try_send(())?;
                    Ok(OwnedFd::from(writer))
                },
                |_| async { Ok(()) },
                Some(selection.transfer()),
            )
            .boxed_local(),
        );
        assert!(transfers.next().now_or_never().is_none());
        opened.recv().await?;
        let (other_work, incoming) = async_channel::bounded(1);
        other_work.send(()).await?;
        tokio::select! {
            result = incoming.recv() => result?,
            _ = transfers.next() => bail!("Stalled transfer unexpectedly completed"),
        }
        drop(transfers);
        let mut buffer = [0_u8; 16 * 1024];
        let drained = loop {
            match reader.read(&mut buffer) {
                Ok(0) => break true,
                Ok(_) => {},
                Err(_) => break false,
            }
        };
        assert!(drained, "Retired transfer kept its native writer alive");
        Ok(())
    }

    #[tokio::test]
    async fn clipboard_transfer_survives_paste_commit_and_only_its_selection_invalidates_it()
    -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let previous = input.begin().context("Previous recording")?;
        let selected = Selection::new("fixture", previous.clone());
        assert!(previous.commit());
        let (writer, mut reader) = UnixStream::pair()?;
        let (sent, reported) = async_channel::bounded(1);
        transfer(
            async { Ok(OwnedFd::from(writer)) },
            move |success| async move {
                sent.send(success).await?;
                Ok(())
            },
            Some(selected.transfer()),
        )
        .await?;
        assert!(
            reported.recv().await?,
            "Paste commitment invalidated clipboard serving"
        );
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        assert_eq!(bytes, b"fixture");

        let current = input.begin().context("Current recording")?;
        let next = Selection::new("fixture", current.clone());
        let previous_payload = selected.transfer();
        drop(selected);
        let (sent, reported) = async_channel::bounded(1);
        let (writer, mut reader) = UnixStream::pair()?;
        reader.set_read_timeout(Some(Duration::from_millis(100)))?;
        transfer(
            async { Ok(OwnedFd::from(writer)) },
            move |success| async move {
                sent.send(success).await?;
                Ok(())
            },
            Some(previous_payload),
        )
        .await?;
        assert!(!reported.recv().await?);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        assert!(
            bytes.is_empty(),
            "Invalidated selection still wrote clipboard data"
        );
        assert!(
            current.active(),
            "An older transfer revoked the new recording"
        );
        assert!(
            !next.invalidated.is_closed(),
            "An older transfer invalidated the new clipboard selection"
        );
        Ok(())
    }

    #[tokio::test]
    async fn stalled_transfer_rpcs_are_bounded() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let selected = Selection::new("fixture", input.begin().context("Recording")?);
        let (sent, reported) = async_channel::bounded(1);
        tokio::time::timeout(
            TRANSFER_TIMEOUT + Duration::from_millis(500),
            transfer(
                std::future::pending(),
                move |success| async move {
                    sent.send(success).await?;
                    Ok(())
                },
                Some(selected.transfer()),
            ),
        )
        .await??;
        assert!(!reported.recv().await?);
        let result = tokio::time::timeout(
            TRANSFER_TIMEOUT + Duration::from_millis(500),
            transfer(std::future::pending(), |_| std::future::pending(), None),
        )
        .await?;
        assert!(result.is_err(), "SelectionWriteDone had no deadline");
        Ok(())
    }

    #[test]
    fn preceding_clipboard_loss_cannot_revoke_or_clear_a_new_selection() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let previous = input.begin().context("Preceding recording")?;
        let next = input.begin().context("Current recording")?;
        let (notify, notices) = async_channel::bounded(4);
        let session: OwnedObjectPath =
            "/org/freedesktop/portal/desktop/session/fixture".try_into()?;
        let mut ownership = SelectionOwnership {
            session: Some(session.clone()),
            monitored: Some(previous),
            owned: true,
        };
        let owner_event = |owned: bool| {
            let values = Results::from([("session_is_owner".into(), OwnedValue::from(owned))]);
            Ok::<_, anyhow::Error>(
                Message::signal(DESKTOP, CLIPBOARD, "SelectionOwnerChanged")?
                    .build(&(session.clone(), values))?,
            )
        };
        ownership.observe(&owner_event(false)?, &notify)?;
        ownership.observe(&owner_event(true)?, &notify)?;
        ownership.adopt(next.clone(), &notify)?;
        let mut payload = Some(Selection::new("fixture", next.clone()));
        apply_notice(notices.try_recv()?, None, &mut payload);
        assert!(
            next.active(),
            "Preceding ownership loss revoked the current recording"
        );
        assert!(
            payload.is_some(),
            "Preceding ownership loss discarded the current payload"
        );
        ownership.observe(&owner_event(false)?, &notify)?;
        apply_notice(notices.try_recv()?, None, &mut payload);
        assert!(!next.commit(), "Current ownership loss authorized paste");
        assert!(payload.is_none());
        let unowned = input.begin().context("Unowned recording")?;
        ownership.adopt(unowned.clone(), &notify)?;
        assert!(
            !unowned.commit(),
            "Unconfirmed clipboard ownership authorized paste"
        );
        Ok(())
    }

    #[test]
    fn shortcut_edges_preserve_double_taps_and_ignore_repeats() {
        let mut held = false;
        assert!(matches!(
            shortcut_event(&mut held, DICTATE_ID, true),
            Some(Input::Press)
        ));
        assert!(shortcut_event(&mut held, DICTATE_ID, true).is_none());
        assert!(matches!(
            shortcut_event(&mut held, DICTATE_ID, false),
            Some(Input::Release)
        ));
        assert!(matches!(
            shortcut_event(&mut held, DICTATE_ID, true),
            Some(Input::Press)
        ));
        assert!(matches!(
            shortcut_event(&mut held, CANCEL_ID, true),
            Some(Input::Cancel)
        ));
        assert!(matches!(
            shortcut_event(&mut held, DICTATE_ID, false),
            Some(Input::Release)
        ));
        assert!(shortcut_event(&mut held, DICTATE_ID, false).is_none());
        assert!(shortcut_event(&mut held, CANCEL_ID, false).is_none());
        assert!(shortcut_event(&mut held, "unknown", true).is_none());
    }
}
