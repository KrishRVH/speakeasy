use super::*;
use anyhow::{Context, ensure};
use futures_util::{FutureExt, StreamExt, future::LocalBoxFuture, stream::FuturesUnordered};
use std::{collections::HashMap, future::Future, os::fd::OwnedFd, sync::Arc, time::Instant};
use zbus::{
    Connection, MatchRule, MessageStream, Proxy,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};
const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const DESKTOP: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
const CLIPBOARD: &str = "org.freedesktop.portal.Clipboard";
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_TRANSFERS: usize = 4;
type Transfers = FuturesUnordered<LocalBoxFuture<'static, anyhow::Result<()>>>;
type Results = HashMap<String, OwnedValue>;
type Options = HashMap<&'static str, Value<'static>>;
type DesktopChanges = futures_util::stream::Select<MessageStream, MessageStream>;
fn portal_owner_rule() -> zbus::Result<MatchRule<'static>> {
    Ok(MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg(DESTINATION)?
        .build())
}
struct Portal {
    connection: Connection,
    requests: Vec<OwnedObjectPath>,
    sessions: Vec<OwnedObjectPath>,
    nonce: String,
    sequence: u64,
}
impl Portal {
    async fn connect() -> anyhow::Result<Self> {
        let connection = Connection::session().await?;
        // The first portal call must identify this host connection. Requesting
        // properties first can cache an empty app ID on recent desktops.
        match connection
            .call_method(
                Some(DESTINATION),
                DESKTOP,
                Some("org.freedesktop.host.portal.Registry"),
                "Register",
                &(APPLICATION_ID, Options::new()),
            )
            .await
        {
            Ok(_) => {}
            Err(zbus::Error::MethodError(name, _, _))
                if matches!(
                    name.as_str(),
                    "org.freedesktop.DBus.Error.UnknownMethod"
                        | "org.freedesktop.DBus.Error.UnknownInterface"
                ) => {}
            Err(error) => return Err(error).context(
                "Install the supplied Speakeasy desktop launcher before enabling portal shortcuts",
            ),
        }
        Ok(Self {
            connection,
            requests: Vec::new(),
            sessions: Vec::new(),
            nonce: format!(
                "s{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            ),
            sequence: 0,
        })
    }
    fn token(&mut self) -> String {
        self.sequence += 1;
        format!("{}_{}", self.nonce, self.sequence)
    }
    async fn proxy(&self, interface: &'static str) -> anyhow::Result<Proxy<'static>> {
        Ok(Proxy::new(&self.connection, DESTINATION, DESKTOP, interface).await?)
    }
    async fn request<B: serde::Serialize + zbus::zvariant::DynamicType>(
        &mut self,
        interface: &'static str,
        method: &'static str,
        token: String,
        body: &B,
    ) -> anyhow::Result<Results> {
        let sender = self
            .connection
            .unique_name()
            .context("D-Bus connection has no name")?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let path: OwnedObjectPath = format!("{DESKTOP}/request/{sender}/{token}").try_into()?;
        let request = Proxy::new(
            &self.connection,
            DESTINATION,
            path.clone(),
            "org.freedesktop.portal.Request",
        )
        .await?;
        let mut responses = request.receive_signal("Response").await?;
        let mut owners =
            MessageStream::for_match_rule(portal_owner_rule()?, &self.connection, Some(4)).await?;
        // Publish ownership before issuing the call, so cancelling consent can
        // close the request even while its response future has not completed.
        self.requests.push(path.clone());
        let returned: OwnedObjectPath = self.proxy(interface).await?.call(method, body).await?;
        ensure!(
            returned == path,
            "Portal returned an unexpected request path"
        );
        let response = tokio::select! {
            response = responses.next() => response.context("Portal request disconnected")?,
            _ = owners.next() => anyhow::bail!("The desktop portal restarted during permission setup"),
        };
        let (status, values): (u32, Results) = response.body().deserialize()?;
        self.requests.retain(|pending| pending != &path);
        ensure!(status == 0, "Desktop permission was declined or cancelled");
        Ok(values)
    }
    async fn session(&mut self, interface: &'static str) -> anyhow::Result<OwnedObjectPath> {
        let handle = self.token();
        let session = self.token();
        let sender = self
            .connection
            .unique_name()
            .context("D-Bus connection has no name")?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let predicted: OwnedObjectPath =
            format!("{DESKTOP}/session/{sender}/{session}").try_into()?;
        self.sessions.push(predicted.clone());
        let options = options([
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
        for (paths, interface) in [
            (&self.requests, "org.freedesktop.portal.Request"),
            (&self.sessions, "org.freedesktop.portal.Session"),
        ] {
            for path in paths {
                let _ = tokio::time::timeout(Duration::from_millis(500), async {
                    let proxy =
                        Proxy::new(&self.connection, DESTINATION, path.clone(), interface).await?;
                    proxy.call::<_, _, ()>("Close", &()).await
                })
                .await;
            }
        }
    }
}
fn options<const N: usize>(values: [(&'static str, Value<'static>); N]) -> Options {
    HashMap::from(values)
}
struct Bound {
    shortcut: Option<OwnedObjectPath>,
    remote: Option<OwnedObjectPath>,
    clipboard: bool,
    sender: Option<ei::Sender>,
    description: String,
    cancel_description: String,
}
struct Selection {
    text: Arc<str>,
    permit: InsertPermit,
    transfers: async_channel::Sender<()>,
    invalidated: async_channel::Receiver<()>,
}
impl Selection {
    fn new(text: &str, permit: InsertPermit) -> Self {
        let (transfers, invalidated) = async_channel::bounded(1);
        Self {
            text: text.into(),
            permit,
            transfers,
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
        self.transfers.close();
    }
}
struct TransferPayload {
    text: Arc<str>,
    invalidated: async_channel::Receiver<()>,
}
struct Preparing<'a> {
    request: Insertion,
    setting_selection: bool,
    task: LocalBoxFuture<'a, anyhow::Result<Prepared>>,
}
enum Prepared {
    SelectionSet,
    Copied(Option<u32>),
    Owned(bool),
}
struct SelectionHandoff {
    permit: InsertPermit,
    ready: async_channel::Sender<bool>,
}
async fn prepare(portal: &mut Portal, settings: &DesktopOptions) -> anyhow::Result<Bound> {
    let mut bound = Bound {
        shortcut: None,
        remote: None,
        clipboard: false,
        sender: None,
        description: settings.shortcut.replace("LOGO", "Super"),
        cancel_description: settings.cancel.replace("LOGO", "Super"),
    };
    if !settings.external_shortcut {
        let session = portal.session(SHORTCUTS).await?;
        let handle = portal.token();
        let shortcuts = vec![
            (
                "dictate",
                options([
                    ("description", Value::from("Hold to dictate")),
                    ("preferred_trigger", Value::from(settings.shortcut.clone())),
                ]),
            ),
            (
                "cancel",
                options([
                    ("description", Value::from("Cancel dictation")),
                    ("preferred_trigger", Value::from(settings.cancel.clone())),
                ]),
            ),
        ];
        let opts = options([("handle_token", Value::from(handle.clone()))]);
        let mut result = portal
            .request(
                SHORTCUTS,
                "BindShortcuts",
                handle,
                &(session.clone(), shortcuts, "", opts),
            )
            .await?;
        let bindings: Vec<(String, Results)> = result
            .remove("shortcuts")
            .context("No shortcut bindings were granted")?
            .try_into()?;
        ensure!(
            bindings.iter().any(|(id, _)| id == "dictate")
                && bindings.iter().any(|(id, _)| id == "cancel"),
            "Both dictate and cancel shortcuts must be enabled"
        );
        for (id, mut values) in bindings {
            if let Some(value) = values.remove("trigger_description") {
                match id.as_str() {
                    "dictate" => bound.description = value.try_into()?,
                    "cancel" => bound.cancel_description = value.try_into()?,
                    _ => {}
                }
            }
        }
        bound.shortcut = Some(session);
    }
    if !settings.manual_paste {
        let tokens = token::TokenStore::open();
        let session = portal.session(REMOTE).await?;
        let handle = portal.token();
        let mut opts = options([
            ("handle_token", Value::from(handle.clone())),
            ("types", Value::from(1_u32)),
        ]);
        if tokens.is_some()
            && portal
                .proxy(REMOTE)
                .await?
                .get_property::<u32>("version")
                .await?
                >= 2
        {
            opts.insert("persist_mode", Value::from(2_u32));
            if let Some(previous) = tokens
                .as_ref()
                .and_then(|store| store.take().ok())
                .flatten()
            {
                opts.insert("restore_token", Value::from(previous));
            }
        }
        portal
            .request(REMOTE, "SelectDevices", handle, &(session.clone(), opts))
            .await?;
        if let Ok(clipboard) = portal.proxy(CLIPBOARD).await
            && clipboard.get_property::<u32>("version").await.is_ok()
        {
            clipboard
                .call::<_, _, ()>("RequestClipboard", &(session.clone(), Options::new()))
                .await?;
            bound.clipboard = true;
        }
        let handle = portal.token();
        let opts = options([("handle_token", Value::from(handle.clone()))]);
        let mut result = portal
            .request(REMOTE, "Start", handle, &(session.clone(), "", opts))
            .await?;
        let devices: u32 = result
            .remove("devices")
            .context("Keyboard permission was not granted")?
            .try_into()?;
        ensure!(devices & 1 != 0, "Keyboard permission was not granted");
        if let Some(store) = tokens
            && let Some(token) = result
                .remove("restore_token")
                .and_then(|value| String::try_from(value).ok())
        {
            // Persistence is optional; a read-only state directory must not
            // discard keyboard access the user has just granted.
            let _ = store.save(&token);
        }
        bound.clipboard &= result
            .remove("clipboard_enabled")
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or(false);
        let descriptor: zbus::zvariant::OwnedFd = portal
            .proxy(REMOTE)
            .await?
            .call("ConnectToEIS", &(session.clone(), Options::new()))
            .await?;
        bound.sender = Some(ei::Sender::connect(OwnedFd::from(descriptor))?);
        bound.remote = Some(session);
    }
    Ok(bound)
}
pub(super) async fn run(
    input: &InputSender,
    settings: &DesktopOptions,
    requests: async_channel::Receiver<Insertion>,
    stop: &async_channel::Receiver<()>,
) -> anyhow::Result<()> {
    if settings.manual_paste && settings.external_shortcut {
        return x11::run(input, settings, requests, stop).await;
    }
    let mut portal = tokio::select! {
        biased;
        _ = stop.recv() => return Ok(()),
        _ = input.sender.closed() => return Ok(()),
        portal = Portal::connect() => portal?,
    };
    let bound = tokio::select! {
        biased;
        _ = stop.recv() => None,
        _ = input.sender.closed() => None,
        ready = prepare(&mut portal, settings) => Some(ready),
    };
    let outcome = match bound {
        Some(Ok(bound)) => serve(&portal, bound, input, settings, requests, stop).await,
        Some(Err(error)) => Err(error),
        None => Ok(()),
    };
    portal.close().await;
    outcome
}
async fn serve(
    portal: &Portal,
    mut bound: Bound,
    input: &InputSender,
    settings: &DesktopOptions,
    requests: async_channel::Receiver<Insertion>,
    stop: &async_channel::Receiver<()>,
) -> anyhow::Result<()> {
    // Keep activation and release on one stream: draining separate queues can
    // reorder a double-tap into two activations followed by two releases.
    let shortcut_rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DESTINATION)?
        .interface(SHORTCUTS)?
        .path(DESKTOP)?
        .build();
    let shortcuts =
        MessageStream::for_match_rule(shortcut_rule, &portal.connection, Some(64)).await?;
    let clipboard = portal.proxy(CLIPBOARD).await?;
    let mut transfers = clipboard.receive_signal("SelectionTransfer").await?;
    let owners = clipboard.receive_signal("SelectionOwnerChanged").await?;
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DESTINATION)?
        .interface("org.freedesktop.portal.Session")?
        .member("Closed")?
        .build();
    let closed = futures_util::stream::select(
        MessageStream::for_match_rule(rule, &portal.connection, Some(4)).await?,
        MessageStream::for_match_rule(portal_owner_rule()?, &portal.connection, Some(4)).await?,
    );
    let (invalidate, invalidated) = async_channel::bounded(64);
    let (selecting, selections) = async_channel::bounded(1);
    let remote_session = bound.remote.clone().filter(|_| bound.clipboard);
    let pump_input = input.clone();
    let shortcut_session = bound.shortcut.clone();
    let sessions = portal.sessions.clone();
    let _pump = Pump(tokio::task::spawn_local(async move {
        let result = pump(
            &pump_input,
            shortcut_session,
            sessions,
            shortcuts,
            closed,
            invalidate,
            owners,
            remote_session,
            selections,
        )
        .await;
        if let Err(error) = result {
            deliver(
                &pump_input,
                Input::Unavailable(format!(
                    "Desktop access stopped: {error}. Resume dictation to reconnect."
                )),
            );
            pump_input.close();
        }
    }));
    let x11_clipboard = if !bound.clipboard {
        Some(x11::ClipboardWorker::new(input.clone())?)
    } else {
        None
    };
    if let Some(worker) = &x11_clipboard {
        tokio::select! {
            biased;
            _ = stop.recv() => return Ok(()),
            _ = input.sender.closed() => return Ok(()),
            ready = worker.ready() => ready?,
        }
    }
    let mut selection: Option<Selection> = None;
    let mut pending: Option<(Insertion, Instant)> = None;
    let mut preparing: Option<Preparing<'_>> = None;
    let mut writing = Transfers::new();
    deliver(
        input,
        Input::DesktopReady {
            shortcut: if settings.external_shortcut {
                "your desktop shortcut".into()
            } else {
                bound.description.clone()
            },
            cancel: if settings.external_shortcut {
                "your cancel shortcut".into()
            } else {
                bound.cancel_description.clone()
            },
        },
    );
    loop {
        while let Ok(notice) = invalidated.try_recv() {
            apply_notice(notice, &mut bound, &mut selection);
        }
        if let Some(sender) = &mut bound.sender {
            sender.dispatch()?;
        }
        if let Some((request, started)) = pending.as_ref() {
            let can_insert = bound.sender.as_ref().is_some_and(|sender| {
                sender.modifiers() == ei::Modifiers::Released && sender.keyboard_available()
            });
            if !request.permit.active()
                || settings.manual_paste
                || request.preserve
                || can_insert
                || started.elapsed() >= MODIFIER_WAIT
            {
                let (request, _) = pending.take().context("Missing insertion request")?;
                if !request.permit.active() {
                    respond(request, Ok(Inserted::Cancelled)).await;
                } else if request.preserve {
                    let result = insert_direct(&mut bound, &request);
                    respond(request, result).await;
                } else {
                    let task = prepare_clipboard(&clipboard, &bound, &x11_clipboard, &request);
                    preparing = Some(Preparing {
                        request,
                        setting_selection: bound.clipboard,
                        task,
                    });
                }
            }
        }
        let native_ready = async {
            match &bound.sender {
                Some(sender) => {
                    sender.ready.readable().await?.clear_ready();
                    Ok::<_, anyhow::Error>(())
                }
                None => std::future::pending().await,
            }
        };
        let deadline = async {
            match pending.as_ref() {
                Some((_, started)) => {
                    tokio::time::sleep_until((*started + MODIFIER_WAIT).into()).await
                }
                None => std::future::pending().await,
            }
        };
        let receive_transfer = bound.clipboard
            && writing.len() < MAX_TRANSFERS
            && !preparing
                .as_ref()
                .is_some_and(|work| work.setting_selection);
        let receive_request = pending.is_none() && preparing.is_none();
        let prepared = async {
            match preparing.as_mut() {
                Some(work) => work.task.as_mut().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            _ = stop.recv() => break,
            _ = input.sender.closed() => break,
            notice = invalidated.recv() => {
                apply_notice(notice.context("Desktop monitoring stopped")?, &mut bound, &mut selection);
            }
            prepared = prepared => {
                let work = preparing.take().context("Missing clipboard preparation")?;
                let request = work.request;
                match prepared {
                    Ok(Prepared::SelectionSet) => {
                        selection = Some(Selection::new(&request.text, request.permit.clone()));
                        let permit = request.permit.clone();
                        let selecting = selecting.clone();
                        let task = async move {
                            let (ready, finished) = async_channel::bounded(1);
                            selecting.send(SelectionHandoff { permit, ready }).await?;
                            Ok(Prepared::Owned(finished.recv().await.context("Clipboard ownership monitoring stopped")?))
                        }.boxed_local();
                        preparing = Some(Preparing { request, setting_selection: false, task });
                    }
                    Ok(Prepared::Copied(Some(owner))) => {
                        let permit = request.permit.clone();
                        let worker = x11_clipboard.as_ref().context("Clipboard is unavailable")?;
                        let task = async move { Ok(Prepared::Owned(worker.owns(owner, permit).await?)) }.boxed_local();
                        preparing = Some(Preparing { request, setting_selection: false, task });
                    }
                    Ok(Prepared::Copied(None)) => {
                        let result = if request.permit.active() {
                            Inserted::Unavailable("Clipboard changed before paste. Dictate again.")
                        } else { Inserted::Cancelled };
                        respond(request, Ok(result)).await;
                    }
                    Ok(Prepared::Owned(owned)) => {
                        // Preparation yields while desktop feedback and ownership
                        // change. Drain both before selecting a binding or committing.
                        while let Ok(notice) = invalidated.try_recv() {
                            apply_notice(notice, &mut bound, &mut selection);
                        }
                        if !owned && bound.clipboard { selection = None; }
                        let result = finish_paste(&mut bound, &request, settings, owned);
                        respond(request, result).await;
                    }
                    Err(error) => respond(request, Err(error)).await,
                }
            }
            completed = writing.next(), if !writing.is_empty() => {
                completed.context("Clipboard transfer stopped")??;
            }
            event = transfers.next(), if receive_transfer => {
                let event = event.context("Clipboard portal disconnected")?;
                let (path, mime, serial): (OwnedObjectPath, String, u32) = event.body().deserialize()?;
                if Some(&path) == bound.remote.as_ref() {
                    let payload = selection.as_ref().filter(|_| mime == "text/plain;charset=utf-8" || mime == "text/plain").map(Selection::transfer);
                    let write_proxy = clipboard.clone();
                    let write_path = path.clone();
                    let done_proxy = clipboard.clone();
                    writing.push(transfer(
                        async move {
                            let fd: zbus::zvariant::OwnedFd = write_proxy.call("SelectionWrite", &(write_path, serial)).await?;
                            Ok(OwnedFd::from(fd))
                        },
                        move |success| async move {
                            done_proxy.call::<_, _, ()>("SelectionWriteDone", &(path, serial, success)).await?;
                            Ok(())
                        },
                        payload,
                    ).boxed_local());
                }
            }
            request = requests.recv(), if receive_request => {
                let Ok(request) = request else { break; };
                pending = Some((request, Instant::now()));
            }
            ready = native_ready => ready?,
            _ = deadline => {}
        }
    }
    if let Some((request, _)) = pending {
        respond(request, Ok(Inserted::Cancelled)).await;
    }
    if let Some(work) = preparing {
        respond(work.request, Ok(Inserted::Cancelled)).await;
    }
    Ok(())
}
struct Pump(tokio::task::JoinHandle<()>);
impl Drop for Pump {
    fn drop(&mut self) {
        self.0.abort();
    }
}
enum Notice {
    Modifiers,
    SelectionLost(InsertPermit),
}
fn apply_notice(notice: Notice, bound: &mut Bound, selection: &mut Option<Selection>) {
    match notice {
        Notice::Modifiers => {
            if let Some(sender) = &mut bound.sender {
                sender.invalidate();
            }
        }
        Notice::SelectionLost(permit) => {
            if selection
                .as_ref()
                .is_some_and(|selection| selection.permit.same_recording(&permit))
            {
                *selection = None;
            }
        }
    }
}
#[expect(
    clippy::too_many_arguments,
    reason = "The input pump owns the native streams and insertion cancellation while clipboard calls await another process"
)]
async fn pump(
    input: &InputSender,
    shortcut: Option<OwnedObjectPath>,
    sessions: Vec<OwnedObjectPath>,
    mut shortcuts: MessageStream,
    mut closed: DesktopChanges,
    invalidate: async_channel::Sender<Notice>,
    mut owners: zbus::proxy::SignalStream<'static>,
    remote: Option<OwnedObjectPath>,
    selections: async_channel::Receiver<SelectionHandoff>,
) -> anyhow::Result<()> {
    let mut selection: Option<InsertPermit> = None;
    let mut owned = false;
    let mut held = false;
    let mut pending: Option<(SelectionHandoff, Instant)> = None;
    let mut pending_events = 0;
    loop {
        if pending
            .as_ref()
            .is_some_and(|(_, started)| owned || started.elapsed() >= Duration::from_millis(500))
        {
            let (handoff, _) = pending.take().context("Missing clipboard handoff")?;
            finish_selection(handoff.permit, owned, &mut selection, &invalidate)?;
            let _ = handoff.ready.try_send(owned);
            pending_events = 0;
        }
        let deadline = async {
            match pending.as_ref() {
                Some((_, started)) => {
                    tokio::time::sleep_until((*started + Duration::from_millis(500)).into()).await
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            _ = input.sender.closed() => return Ok(()),
            handoff = selections.recv(), if pending.is_none() => {
                let handoff = handoff.context("Clipboard preparation stopped")?;
                // SetSelection has completed. Process its queued ownership
                // signals against the preceding permit before changing owners.
                let mut drained = 0;
                for _ in 0..64 {
                    let Some(event) = owners.next().now_or_never() else { break; };
                    let event = event.context("Clipboard portal disconnected")?;
                    observe_owner(&event, remote.as_ref(), &mut selection, &mut owned, &invalidate)?;
                    drained += 1;
                }
                ensure!(drained < 64 || owners.next().now_or_never().is_none(), "Clipboard ownership queue overran");
                pending = Some((handoff, Instant::now()));
            }
            event = owners.next(), if remote.is_some() => {
                let event = event.context("Clipboard portal disconnected")?;
                observe_owner(&event, remote.as_ref(), &mut selection, &mut owned, &invalidate)?;
                if pending.is_some() {
                    pending_events += 1;
                    ensure!(pending_events <= 64, "Clipboard ownership queue overran");
                }
            }
            event = closed.next() => {
                let event = event.context("Portal session monitoring stopped")??;
                if event.header().member().is_some_and(|member| member.as_str() == "NameOwnerChanged") {
                    anyhow::bail!("The desktop portal restarted");
                }
                if sessions.iter().any(|path| event.header().path().is_some_and(|closed| closed.as_str() == path.as_str())) {
                    anyhow::bail!("Desktop permissions were revoked");
                }
            }
            event = shortcuts.next(), if shortcut.is_some() => {
                let event = event.context("Shortcut portal disconnected")??;
                let header = event.header();
                let Some(member) = header.member().filter(|member| matches!(member.as_str(), "Activated" | "Deactivated")) else { continue; };
                let (path, action, _, _): (OwnedObjectPath, String, u64, Results) = event.body().deserialize()?;
                if Some(&path) == shortcut.as_ref() && let Some(event) = shortcut_event(&mut held, &action, member.as_str() == "Activated") {
                    if matches!(event, Input::Press) && invalidate.try_send(Notice::Modifiers).is_err() { input.close(); return Ok(()); }
                    deliver(input, event);
                }
            }
            _ = deadline => {}
        }
    }
}
fn observe_owner(
    event: &zbus::Message,
    remote: Option<&OwnedObjectPath>,
    selection: &mut Option<InsertPermit>,
    owned: &mut bool,
    invalidate: &async_channel::Sender<Notice>,
) -> anyhow::Result<()> {
    let (path, values): (OwnedObjectPath, Results) = event.body().deserialize()?;
    if Some(&path) == remote {
        *owned = values
            .get("session_is_owner")
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or(false);
        if !*owned && let Some(permit) = selection.take() {
            permit.revoke();
            invalidate.try_send(Notice::SelectionLost(permit))?;
        }
    }
    Ok(())
}
fn finish_selection(
    permit: InsertPermit,
    owned: bool,
    selection: &mut Option<InsertPermit>,
    invalidate: &async_channel::Sender<Notice>,
) -> anyhow::Result<()> {
    if owned {
        *selection = Some(permit);
    } else {
        permit.revoke();
        invalidate.try_send(Notice::SelectionLost(permit))?;
    }
    Ok(())
}
fn shortcut_event(held: &mut bool, action: &str, activated: bool) -> Option<Input> {
    match (action, activated) {
        ("dictate", true) if !*held => {
            *held = true;
            Some(Input::Press)
        }
        ("dictate", false) if std::mem::take(held) => Some(Input::Release),
        ("cancel", true) => Some(Input::Cancel),
        _ => None,
    }
}
fn prepare_clipboard<'a>(
    proxy: &Proxy<'static>,
    bound: &Bound,
    clipboard: &'a Option<x11::ClipboardWorker>,
    request: &Insertion,
) -> LocalBoxFuture<'a, anyhow::Result<Prepared>> {
    let permit = request.permit.clone();
    if bound.clipboard {
        let session = bound.remote.clone();
        let proxy = proxy.clone();
        async move {
            let session = session.context("Clipboard has no desktop session")?;
            let opts = options([(
                "mime_types",
                Value::from(vec!["text/plain;charset=utf-8", "text/plain"]),
            )]);
            if !permit.active() {
                return Ok(Prepared::Copied(None));
            }
            tokio::time::timeout(
                Duration::from_secs(2),
                proxy.call::<_, _, ()>("SetSelection", &(session, opts)),
            )
            .await??;
            Ok(Prepared::SelectionSet)
        }
        .boxed_local()
    } else {
        let text = request.text.clone();
        async move {
            let worker = clipboard.as_ref().context("Clipboard is unavailable")?;
            Ok(Prepared::Copied(worker.set_text(text, permit).await?))
        }
        .boxed_local()
    }
}
fn insert_direct(bound: &mut Bound, request: &Insertion) -> anyhow::Result<Inserted> {
    if let Some(sender) = &mut bound.sender {
        sender.dispatch()?;
    }
    let Some(sender) = bound
        .sender
        .as_mut()
        .filter(|sender| sender.text_available())
    else {
        return Ok(Inserted::Unavailable(
            "This desktop does not support direct text input. Turn off Keep clipboard to use paste.",
        ));
    };
    if !request.permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    sender.text(&request.text)?;
    Ok(Inserted::Sent)
}
fn finish_paste(
    bound: &mut Bound,
    request: &Insertion,
    settings: &DesktopOptions,
    owned: bool,
) -> anyhow::Result<Inserted> {
    if let Some(sender) = &mut bound.sender {
        sender.dispatch()?;
    }
    if !request.permit.active() {
        return Ok(Inserted::Cancelled);
    }
    if !owned {
        return Ok(Inserted::Unavailable(if bound.clipboard {
            "Clipboard ownership could not be confirmed. Dictate again or choose Copy for manual paste."
        } else {
            "Clipboard changed before paste. Dictate again."
        }));
    }
    let Some(sender) = bound.sender.as_mut().filter(|sender| {
        sender.keyboard_available()
            && sender.modifiers() == ei::Modifiers::Released
            && !settings.manual_paste
    }) else {
        return Ok(Inserted::Copied(
            "Text copied. This desktop could not confirm released shortcut keys; paste manually.",
        ));
    };
    if !request.permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    sender.paste(settings.terminal_paste)?;
    Ok(Inserted::Sent)
}
async fn transfer<D: Future<Output = anyhow::Result<()>>>(
    write: impl Future<Output = anyhow::Result<OwnedFd>>,
    done: impl FnOnce(bool) -> D,
    payload: Option<TransferPayload>,
) -> anyhow::Result<()> {
    let success = if let Some(payload) = payload {
        // Selection lifetime is separate from the paste commit gate: editors
        // may request this payload after insertion has already committed.
        tokio::select! {
            biased;
            _ = payload.invalidated.recv() => false,
            result = async {
                let fd = tokio::time::timeout(TRANSFER_TIMEOUT, write).await??;
                write_transfer(fd, payload.text.as_bytes()).await
            } => result.is_ok(),
        }
    } else {
        false
    };
    tokio::time::timeout(TRANSFER_TIMEOUT, done(success)).await??;
    Ok(())
}
async fn write_transfer(fd: OwnedFd, bytes: &[u8]) -> anyhow::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: fd is owned and remains alive; fcntl only changes its I/O flags.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        ensure!(
            flags >= 0 && libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0,
            "Cannot prepare clipboard transfer"
        );
    }
    let ready = tokio::io::unix::AsyncFd::new(fd)?;
    tokio::time::timeout(TRANSFER_TIMEOUT, async {
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let mut writable = ready.writable().await?;
            let result = writable.try_io(|fd| {
                // SAFETY: buffer is live, fd is owned, and the kernel reads at
                // most this slice length during the nonblocking write.
                let written = unsafe {
                    libc::write(
                        fd.get_ref().as_raw_fd(),
                        remaining.as_ptr().cast(),
                        remaining.len(),
                    )
                };
                if written < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(written as usize)
                }
            });
            if let Ok(written) = result {
                let written = written?;
                ensure!(written > 0, "Clipboard transfer stopped");
                remaining = &remaining[written..];
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_transfers_allow_new_work_and_retire_without_detached_writes()
    -> anyhow::Result<()> {
        let (events, _) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let permit = input.begin().context("Recording")?;
        let selection = Selection::new(&"fixture".repeat(128 * 1024), permit);
        let (writer, mut reader) = std::os::unix::net::UnixStream::pair()?;
        reader.set_read_timeout(Some(Duration::from_millis(100)))?;
        let (entered, writing) = async_channel::bounded(1);
        let mut transfers = Transfers::new();
        transfers.push(
            transfer(
                async move {
                    let _ = entered.try_send(());
                    Ok(OwnedFd::from(writer))
                },
                |_| async { Ok(()) },
                Some(selection.transfer()),
            )
            .boxed_local(),
        );
        assert!(transfers.next().now_or_never().is_none());
        writing.recv().await?;
        // Production transfers stay pending while the desktop service can
        // receive stop/EI/insertion messages rather than await the stalled pipe.
        let (next, incoming) = async_channel::bounded(1);
        next.send(()).await?;
        tokio::select! {
            result = incoming.recv() => result?,
            _ = transfers.next() => anyhow::bail!("Stalled transfer unexpectedly completed"),
        }
        drop(transfers);
        use std::io::Read;
        let mut buffer = [0_u8; 16 * 1024];
        let drained = loop {
            match reader.read(&mut buffer) {
                Ok(0) => break true,
                Ok(_) => {}
                Err(_) => break false,
            }
        };
        assert!(drained, "Retired transfer kept its native writer alive");
        Ok(())
    }

    #[tokio::test]
    async fn clipboard_transfer_survives_paste_commit_and_only_its_selection_invalidates_it()
    -> anyhow::Result<()> {
        let (events, _) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let previous = input.begin().context("Previous recording")?;
        let selected = Selection::new("fixture", previous.clone());
        assert!(previous.commit());
        let (writer, mut reader) = std::os::unix::net::UnixStream::pair()?;
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
        use std::io::Read;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        assert_eq!(bytes, b"fixture");

        let current = input.begin().context("Current recording")?;
        let next = Selection::new("fixture", current.clone());
        let previous_payload = selected.transfer();
        drop(selected);
        let (sent, reported) = async_channel::bounded(1);
        let (writer, mut reader) = std::os::unix::net::UnixStream::pair()?;
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
        let (events, _) = async_channel::bounded(1);
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
        let (events, _) = async_channel::bounded(1);
        let input = InputSender::new(events);
        let previous = input.begin().context("Preceding recording")?;
        let next = input.begin().context("Current recording")?;
        let (invalidate, notices) = async_channel::bounded(4);
        let remote: OwnedObjectPath =
            "/org/freedesktop/portal/desktop/session/fixture".try_into()?;
        let mut monitored = Some(previous);
        let mut owned = true;
        let owner_event = |owned: bool| {
            let values = Results::from([("session_is_owner".into(), OwnedValue::from(owned))]);
            Ok::<_, anyhow::Error>(
                zbus::Message::signal(DESKTOP, CLIPBOARD, "SelectionOwnerChanged")?
                    .build(&(remote.clone(), values))?,
            )
        };
        observe_owner(
            &owner_event(false)?,
            Some(&remote),
            &mut monitored,
            &mut owned,
            &invalidate,
        )?;
        observe_owner(
            &owner_event(true)?,
            Some(&remote),
            &mut monitored,
            &mut owned,
            &invalidate,
        )?;
        finish_selection(next.clone(), owned, &mut monitored, &invalidate)?;
        let mut bound = Bound {
            shortcut: None,
            remote: Some(remote.clone()),
            clipboard: true,
            sender: None,
            description: String::new(),
            cancel_description: String::new(),
        };
        let mut payload = Some(Selection::new("fixture", next.clone()));
        apply_notice(notices.try_recv()?, &mut bound, &mut payload);
        assert!(
            next.active(),
            "Preceding ownership loss revoked the current recording"
        );
        assert!(
            payload.is_some(),
            "Preceding ownership loss discarded the current payload"
        );
        observe_owner(
            &owner_event(false)?,
            Some(&remote),
            &mut monitored,
            &mut owned,
            &invalidate,
        )?;
        apply_notice(notices.try_recv()?, &mut bound, &mut payload);
        assert!(!next.commit(), "Current ownership loss authorized paste");
        assert!(payload.is_none());
        let unowned = input.begin().context("Unowned recording")?;
        finish_selection(unowned.clone(), false, &mut monitored, &invalidate)?;
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
            shortcut_event(&mut held, "dictate", true),
            Some(Input::Press)
        ));
        assert!(shortcut_event(&mut held, "dictate", true).is_none());
        assert!(matches!(
            shortcut_event(&mut held, "dictate", false),
            Some(Input::Release)
        ));
        assert!(matches!(
            shortcut_event(&mut held, "dictate", true),
            Some(Input::Press)
        ));
        assert!(matches!(
            shortcut_event(&mut held, "cancel", true),
            Some(Input::Cancel)
        ));
        assert!(matches!(
            shortcut_event(&mut held, "dictate", false),
            Some(Input::Release)
        ));
        assert!(shortcut_event(&mut held, "dictate", false).is_none());
        assert!(shortcut_event(&mut held, "cancel", false).is_none());
        assert!(shortcut_event(&mut held, "unknown", true).is_none());
    }
}
