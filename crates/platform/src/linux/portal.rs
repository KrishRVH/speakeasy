//! Wayland desktop access through xdg-desktop-portal: global shortcuts, keyboard input over libei,
//! and the portal clipboard. Every request and session the service opens is closed on retirement.

mod insertion;
mod transfer;

use std::{
    collections::HashMap,
    convert::Infallible,
    iter,
    ops::ControlFlow,
    os::fd::OwnedFd,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail, ensure};
use async_channel::{Receiver, Sender};
use futures_util::{FutureExt, StreamExt};
use tokio::task::JoinSet;
use zbus::{
    Connection, MatchRule, Message, MessageStream, Proxy,
    names::MemberName,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use self::insertion::Insertions;
use super::{APPLICATION_ID, ShortcutLabels, ei, owner_changes, stopping, token::TokenStore, x11};
use crate::{
    DesktopOptions, Input, InputSender, InsertPermit, LinuxPaste,
    insertion::{Insertion, Reply},
};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const DESKTOP: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
const CLIPBOARD: &str = "org.freedesktop.portal.Clipboard";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";
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
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);
const OWNERSHIP_SETTLE: Duration = Duration::from_millis(500);

type Results = HashMap<String, OwnedValue>;
type Options = HashMap<&'static str, Value<'static>>;
type DesktopChanges = futures_util::stream::Select<MessageStream, MessageStream>;

struct Portal {
    connection: Connection,
    /// This connection's unique bus name as it appears in portal object paths.
    sender_segment: String,
    unanswered: Vec<OwnedObjectPath>,
    sessions: Vec<OwnedObjectPath>,
    /// Prefixes every request and session token. org.freedesktop.portal.Request asks for tokens
    /// that are unique and not guessable: the bus name and `sequence` keep them unique, and this
    /// process ID and connection time keep them hard to guess.
    nonce: String,
    sequence: u64,
}

impl Portal {
    /// Connects and registers, then subscribes to the session closures and portal restarts that
    /// revoke desktop access, before any session exists. Registration comes first because its call
    /// starts an inactive portal, which would otherwise read as a restart.
    async fn connect() -> anyhow::Result<(Self, DesktopChanges)> {
        let connection = Connection::session().await?;
        // The host registry accepts registration only before any other portal call.
        let registered = connection
            .call_method(
                Some(DESTINATION),
                DESKTOP,
                Some("org.freedesktop.host.portal.Registry"),
                "Register",
                &(APPLICATION_ID, Options::new()),
            )
            .await;
        if let Err(error) = registered
            && !lacks_registry(&error)
        {
            return Err(error).context(
                "Install the supplied Speakeasy desktop launcher before enabling portal shortcuts",
            );
        }
        let sender_segment = connection
            .unique_name()
            .context("D-Bus connection has no name")?
            .trim_start_matches(':')
            .replace('.', "_");
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let changes = desktop_changes(&connection).await?;
        let portal = Self {
            connection,
            sender_segment,
            unanswered: Vec::new(),
            sessions: Vec::new(),
            nonce: format!("s{}_{now}", std::process::id()),
            sequence: 0,
        };
        Ok((portal, changes))
    }

    /// The path prefix of every session this connection creates.
    fn session_root(&self) -> String {
        format!("{DESKTOP}/session/{}/", self.sender_segment)
    }

    fn next_token(&mut self) -> String {
        self.sequence = self.sequence.wrapping_add(1);
        format!("{}_{}", self.nonce, self.sequence)
    }

    /// A new request token and the options that carry it; the portal names the request object
    /// after that token, which is the path [`Self::request`] awaits.
    fn request_options(&mut self) -> (String, Options) {
        let handle = self.next_token();
        let options = Options::from([("handle_token", Value::from(handle.clone()))]);
        (handle, options)
    }

    async fn proxy(&self, interface: &'static str) -> anyhow::Result<Proxy<'static>> {
        Ok(Proxy::new(&self.connection, DESTINATION, DESKTOP, interface).await?)
    }

    /// Calls a portal method that answers through a Request object and awaits that answer. A portal
    /// restart leaves the request unanswered; binding, its only caller, watches for restarts.
    async fn request<B: serde::Serialize + zbus::zvariant::DynamicType + Sync>(
        &mut self,
        interface: &'static str,
        method: &'static str,
        token: String,
        body: &B,
    ) -> anyhow::Result<Results> {
        let path: OwnedObjectPath =
            format!("{DESKTOP}/request/{}/{token}", self.sender_segment).try_into()?;
        let request = Proxy::new(&self.connection, DESTINATION, path.clone(), REQUEST).await?;
        let mut responses = request.receive_signal("Response").await?;
        // Track the request before calling, so retirement can close a consent dialog that has
        // not answered yet.
        self.unanswered.push(path.clone());
        let returned: OwnedObjectPath = self.proxy(interface).await?.call(method, body).await?;
        ensure!(
            returned == path,
            "Portal returned an unexpected request path"
        );
        let response = responses
            .next()
            .await
            .context("Portal request disconnected")?;
        let (status, values): (u32, Results) = response.body().deserialize()?;
        self.unanswered.retain(|pending| pending != &path);
        ensure!(status == 0, "Desktop permission was declined or cancelled");
        Ok(values)
    }

    async fn create_session(&mut self, interface: &'static str) -> anyhow::Result<OwnedObjectPath> {
        let (handle, mut options) = self.request_options();
        let session = self.next_token();
        let predicted: OwnedObjectPath = format!("{}{session}", self.session_root()).try_into()?;
        self.sessions.push(predicted.clone());
        options.insert("session_handle_token", Value::from(session));
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
                let _ = tokio::time::timeout(
                    CLOSE_TIMEOUT,
                    self.connection.call_method(
                        Some(DESTINATION),
                        path,
                        Some(interface),
                        "Close",
                        &(),
                    ),
                )
                .await;
            }
        }
    }
}

/// What binding established: the shortcut session and the remote desktop for keyboard input that
/// the portal granted, and the shortcut labels Settings shows.
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

/// What the pump reports to the insertion loop.
enum Notice {
    /// The dictation shortcut was pressed, so libei's last modifier report is stale.
    ModifiersChanged,
    /// This session does not own the portal selection holding this recording's text.
    SelectionLost {
        permit: InsertPermit,
        /// Whether this loss is what revoked the recording, as `InsertPermit::forfeit` reports.
        revoked: bool,
    },
}

/// Watches shortcuts, session revocation, and clipboard ownership while insertion awaits other
/// processes, revoking a permit as soon as its selection is lost.
struct Pump {
    input: InputSender,
    shortcut_session: Option<OwnedObjectPath>,
    /// The prefix of every session this connection opened, as `Portal::session_root` gives it.
    session_root: String,
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
    async fn watch(mut self) {
        if let Err(error) = self.run().await {
            self.input.fail(format!(
                "Desktop access stopped: {error}. Resume dictation to reconnect."
            ));
        }
    }

    async fn run(&mut self) -> anyhow::Result<()> {
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
                Err(revoked) = revocation(&mut self.changes, &self.session_root) => {
                    return Err(revoked);
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
        let mut queued = iter::from_fn(|| self.owners.next().now_or_never()).fuse();
        for event in queued.by_ref().take(SIGNAL_BACKLOG) {
            self.ownership.observe(
                &event.context("Clipboard portal disconnected")??,
                &self.notify,
            )?;
        }
        ensure!(queued.next().is_none(), "Clipboard ownership queue overran");
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

    fn on_shortcut(&mut self, event: &Message) -> anyhow::Result<ControlFlow<()>> {
        let activated = match event.header().member().map(MemberName::as_str) {
            Some("Activated") => true,
            Some("Deactivated") => false,
            _ => return Ok(ControlFlow::Continue(())),
        };
        let (path, action, _, _): (OwnedObjectPath, String, u64, Results) =
            event.body().deserialize()?;
        if Some(&path) != self.shortcut_session.as_ref() {
            return Ok(ControlFlow::Continue(()));
        }
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
    if options.paste == LinuxPaste::Manual && options.external_shortcut {
        return x11::run(input, options, requests, stop).await;
    }
    let (mut portal, mut changes) = tokio::select! {
        biased;
        () = stopping(stop, input) => return Ok(()),
        connected = Portal::connect() => connected?,
    };
    // A portal restart, or a session closed while a later consent dialog is open, ends binding,
    // which closes that dialog. Binding is polled before revocation: a declined dialog answers
    // before the portal closes its session, and that answer names the cause.
    let session_root = portal.session_root();
    let bound = tokio::select! {
        biased;
        () = stopping(stop, input) => None,
        bound = bind(&mut portal, options) => Some(bound),
        Err(revoked) = revocation(&mut changes, &session_root) => Some(Err(revoked)),
    };
    let outcome = match bound {
        Some(Ok(bound)) => serve(&portal, bound, changes, input, options, requests, stop).await,
        Some(Err(error)) => Err(error),
        None => Ok(()),
    };
    portal.close().await;
    outcome
}

async fn bind(portal: &mut Portal, options: &DesktopOptions) -> anyhow::Result<Bound> {
    // A host without libei fails here, before the user is asked for permissions it cannot use.
    let libei = (options.paste != LinuxPaste::Manual)
        .then(ei::Api::load)
        .transpose()?;
    let (shortcut_session, labels) = if options.external_shortcut {
        (None, ShortcutLabels::new(options))
    } else {
        let (session, labels) = bind_shortcuts(portal, options).await?;
        (Some(session), labels)
    };
    let remote = match libei {
        Some(libei) => Some(start_remote_desktop(portal, libei, options.paste).await?),
        None => None,
    };
    Ok(Bound {
        shortcut_session,
        remote,
        labels,
    })
}

/// Binds the dictate and cancel shortcuts, returning their session and the labels Settings shows.
async fn bind_shortcuts(
    portal: &mut Portal,
    options: &DesktopOptions,
) -> anyhow::Result<(OwnedObjectPath, ShortcutLabels)> {
    let session = portal.create_session(SHORTCUTS).await?;
    let (handle, request) = portal.request_options();
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
    let mut result = portal
        .request(
            SHORTCUTS,
            "BindShortcuts",
            handle,
            &(&session, shortcuts, "", request),
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
    let mut labels = ShortcutLabels::new(options);
    for (id, mut values) in bindings {
        if let Some(trigger) = values.remove("trigger_description") {
            match id.as_str() {
                DICTATE_ID => labels.shortcut = trigger.try_into()?,
                CANCEL_ID => labels.cancel = trigger.try_into()?,
                _ => {},
            }
        }
    }
    Ok((session, labels))
}

async fn start_remote_desktop(
    portal: &mut Portal,
    libei: ei::Api,
    paste: LinuxPaste,
) -> anyhow::Result<Remote> {
    let tokens = TokenStore::open();
    let session = portal.create_session(REMOTE).await?;
    let (handle, mut devices) = portal.request_options();
    devices.insert("types", Value::from(KEYBOARD_DEVICE));
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
        .request(REMOTE, "SelectDevices", handle, &(&session, devices))
        .await?;
    let clipboard_requested = request_clipboard(portal, &session).await?;
    let (handle, start) = portal.request_options();
    let mut result = portal
        .request(REMOTE, "Start", handle, &(&session, "", start))
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
        .call("ConnectToEIS", &(&session, Options::new()))
        .await?;
    Ok(Remote {
        session,
        sender: ei::Sender::connect(
            libei,
            OwnedFd::from(descriptor),
            paste == LinuxPaste::Terminal,
        )?,
        shares_clipboard,
    })
}

async fn request_clipboard(portal: &Portal, session: &OwnedObjectPath) -> anyhow::Result<bool> {
    let clipboard = portal.proxy(CLIPBOARD).await?;
    // Creating a proxy does not contact the portal; reading `version` proves the interface exists.
    if clipboard.get_property::<u32>("version").await.is_err() {
        return Ok(false);
    }
    clipboard
        .call::<_, _, ()>("RequestClipboard", &(session, Options::new()))
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
    changes: DesktopChanges,
    input: &InputSender,
    options: &DesktopOptions,
    requests: Receiver<Insertion>,
    stop: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    let shortcuts = shortcut_signals(&portal.connection).await?;
    let clipboard = portal.proxy(CLIPBOARD).await?;
    let transfer_requests = clipboard.receive_signal("SelectionTransfer").await?;
    let owners = selection_owner_changes(&portal.connection).await?;
    let (notify, notices) = async_channel::bounded(SIGNAL_BACKLOG);
    let (selecting, selections) = async_channel::bounded(1);
    // Dropping the set aborts the pump once serving ends.
    let mut pump = JoinSet::new();
    pump.spawn_local(
        Pump {
            input: input.clone(),
            shortcut_session: bound.shortcut_session.clone(),
            session_root: portal.session_root(),
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
    // Focus checks need the X worker, which touches X clipboard contents only for X11 copies,
    // never for portal copies or direct text.
    let worker = x11::ClipboardWorker::spawn(input.clone())?;
    tokio::select! {
        biased;
        () = stopping(stop, input) => return Ok(()),
        ready = worker.ready() => ready?,
    }
    input.deliver(bound.labels.clone().into_desktop_ready());
    Insertions::new(
        &mut bound, &notices, &selecting, &worker, &clipboard, options,
    )
    .serve(transfer_requests, requests, input, stop)
    .await
}

/// Whether a registration failed because the portal predates the host registry and identifies the
/// app from its launcher instead. zbus reports a method call's error reply by its D-Bus error name.
fn lacks_registry(error: &zbus::Error) -> bool {
    matches!(
        error,
        zbus::Error::MethodError(name, _, _)
            if matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.UnknownMethod"
                    | "org.freedesktop.DBus.Error.UnknownInterface"
            )
    )
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

/// Fails with why desktop access ended once a session under `session_root` closes, the portal
/// restarts, or monitoring stops.
async fn revocation(
    changes: &mut DesktopChanges,
    session_root: &str,
) -> anyhow::Result<Infallible> {
    while let Some(event) = changes.next().await {
        check_revocation(&event?, session_root)?;
    }
    bail!("Portal session monitoring stopped")
}

/// Fails when `event` revokes desktop access: a portal restart, or the closure of a session under
/// `session_root`. Another connection's sessions sit under another root.
fn check_revocation(event: &Message, session_root: &str) -> anyhow::Result<()> {
    let header = event.header();
    if header
        .member()
        .is_some_and(|member| member.as_str() == "NameOwnerChanged")
    {
        bail!("The desktop portal restarted");
    }
    if header
        .path()
        .is_some_and(|closed| closed.as_str().starts_with(session_root))
    {
        bail!("Desktop permissions were revoked");
    }
    Ok(())
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

/// Revokes `permit`, whose selection this session does not own, and tells the insertion loop. The
/// pump shares that loop's thread and sends with no await after revoking, and the loop applies
/// queued notices before it pastes, so a paste this loss revoked always learns why.
fn lose_selection(permit: InsertPermit, notify: &Sender<Notice>) -> anyhow::Result<()> {
    let revoked = permit.forfeit();
    notify.try_send(Notice::SelectionLost { permit, revoked })?;
    Ok(())
}

fn shortcut_event(held: &mut bool, action: &str, activated: bool) -> Option<Input> {
    match action {
        DICTATE_ID if *held != activated => {
            *held = activated;
            Some(if activated {
                Input::Press
            } else {
                Input::Release
            })
        },
        CANCEL_ID if activated => Some(Input::Cancel),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{insertion::apply_notice, transfer::Selection, *};

    pub(super) fn fixture_session() -> anyhow::Result<OwnedObjectPath> {
        Ok(format!("{DESKTOP}/session/fixture").try_into()?)
    }

    /// The portal announcing whether `session` owns the clipboard selection.
    pub(super) fn owner_changed(session: &OwnedObjectPath, owned: bool) -> anyhow::Result<Message> {
        let values = Results::from([("session_is_owner".into(), OwnedValue::from(owned))]);
        let signal = Message::signal(DESKTOP, CLIPBOARD, "SelectionOwnerChanged")?;
        Ok(signal.build(&(session, values))?)
    }

    #[test]
    fn only_a_portal_without_the_host_registry_skips_registration() -> anyhow::Result<()> {
        let call = Message::method_call(DESKTOP, "Register")?
            .interface("org.freedesktop.host.portal.Registry")?
            .build(&())?;
        for (name, lacks) in [
            ("org.freedesktop.DBus.Error.UnknownMethod", true),
            ("org.freedesktop.DBus.Error.UnknownInterface", true),
            ("org.freedesktop.DBus.Error.AccessDenied", false),
            ("org.freedesktop.portal.Error.Failed", false),
        ] {
            let reply =
                zbus::Error::from(Message::error(&call.header(), name)?.build(&("refused",))?);
            assert_eq!(lacks_registry(&reply), lacks, "{name}");
        }
        Ok(())
    }

    #[test]
    fn only_this_connections_sessions_and_portal_restarts_revoke_access() -> anyhow::Result<()> {
        let root = format!("{DESKTOP}/session/1_23/");
        let closed = |path: &str| -> anyhow::Result<Message> {
            Ok(Message::signal(path, SESSION, "Closed")?.build(&())?)
        };
        assert!(check_revocation(&closed(&format!("{root}s1_2"))?, &root).is_err());
        assert!(
            check_revocation(&closed(&format!("{DESKTOP}/session/1_234/s1_2"))?, &root).is_ok(),
            "Another connection's session closure revoked access"
        );
        let restarted = Message::signal(
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameOwnerChanged",
        )?
        .build(&(DESTINATION, ":1.7", ":1.8"))?;
        assert!(check_revocation(&restarted, &root).is_err());
        Ok(())
    }

    #[test]
    fn preceding_clipboard_loss_cannot_revoke_or_clear_a_new_selection() -> anyhow::Result<()> {
        let (events, _receiver) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let previous = input.begin().context("Preceding recording")?;
        let next = input.begin().context("Current recording")?;
        let (notify, notices) = async_channel::bounded(4);
        let session = fixture_session()?;
        let mut ownership = SelectionOwnership {
            session: Some(session.clone()),
            monitored: Some(previous),
            owned: true,
        };
        ownership.observe(&owner_changed(&session, false)?, &notify)?;
        ownership.observe(&owner_changed(&session, true)?, &notify)?;
        ownership.adopt(next.clone(), &notify)?;
        let mut payload = Some(Selection::new("fixture", next.clone()));
        let mut forfeited = None;
        apply_notice(notices.try_recv()?, None, &mut payload, &mut forfeited);
        assert!(
            next.active(),
            "Preceding ownership loss revoked the current recording"
        );
        assert!(
            payload.is_some(),
            "Preceding ownership loss discarded the current payload"
        );
        ownership.observe(&owner_changed(&session, false)?, &notify)?;
        apply_notice(notices.try_recv()?, None, &mut payload, &mut forfeited);
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
