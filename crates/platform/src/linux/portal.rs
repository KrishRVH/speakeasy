mod insertion;
mod transfer;

use std::{collections::HashMap, convert::Infallible, time::Duration};

use anyhow::{Context as _, bail, ensure};
use async_channel::Receiver;
use futures_util::StreamExt as _;
use tokio::task::JoinSet;
use zbus::{
    Connection, MatchRule, Message, MessageStream, Proxy,
    names::MemberName,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use self::insertion::Insertions;
use super::{
    APPLICATION_ID, PasteKeys, ShortcutLabels, ei, owner_changes, stop_requested,
    token::TokenStore, x11,
};
use crate::{DesktopOptions, Input, InputSender, LinuxPaste, insertion::Insertion};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const DESKTOP: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
const CLIPBOARD: &str = "org.freedesktop.portal.Clipboard";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";

const DICTATE_ID: &str = "dictate";
const CANCEL_ID: &str = "cancel";
const KEYBOARD_DEVICE: u32 = 1;
const PERSIST_UNTIL_REVOKED: u32 = 2;
const PERSISTENT_REMOTE_VERSION: u32 = 2;

const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

type Results = HashMap<String, OwnedValue>;
type Options<'a> = HashMap<&'static str, Value<'a>>;
type DesktopChanges = futures_util::stream::Select<MessageStream, MessageStream>;

struct Portal {
    connection: Connection,
    sender_segment: String,
    unanswered: Option<OwnedObjectPath>,
    sessions: Vec<OwnedObjectPath>,
    sequence: u64,
}

impl Portal {
    async fn connect() -> anyhow::Result<(Self, DesktopChanges)> {
        let connection = Connection::session().await?;
        let changes = watch_portal(&connection).await?;
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
        let portal = Self {
            connection,
            sender_segment,
            unanswered: None,
            sessions: Vec::new(),
            sequence: 0,
        };
        Ok((portal, changes))
    }

    fn session_root(&self) -> String {
        format!("{DESKTOP}/session/{}/", self.sender_segment)
    }

    fn next_token(&mut self) -> String {
        self.sequence = self.sequence.wrapping_add(1);
        format!("s{}", self.sequence)
    }

    fn request_options(&mut self) -> (String, Options<'static>) {
        let handle = self.next_token();
        let options = Options::from([("handle_token", Value::from(handle.clone()))]);
        (handle, options)
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
            format!("{DESKTOP}/request/{}/{token}", self.sender_segment).try_into()?;
        let request = Proxy::new(&self.connection, DESTINATION, path.clone(), REQUEST).await?;
        let mut responses = request.receive_signal("Response").await?;

        self.unanswered = Some(path.clone());
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
        self.unanswered = None;
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
        for (paths, interface) in [
            (self.unanswered.as_slice(), REQUEST),
            (&self.sessions, SESSION),
        ] {
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

struct Bound {
    shortcut_session: Option<OwnedObjectPath>,
    sender: Option<ei::Sender>,
    clipboard_session: Option<OwnedObjectPath>,
    labels: ShortcutLabels,
    paste: LinuxPaste,
}

async fn watch_shortcuts(
    input: InputSender,
    mut shortcuts: Option<MessageStream>,
    mut changes: DesktopChanges,
    session_root: String,
) {
    if let Err(error) = read_shortcuts(&input, &mut shortcuts, &mut changes, &session_root).await {
        input.fail(format!(
            "Desktop access stopped: {error}. Resume dictation to reconnect."
        ));
    }
}

async fn read_shortcuts(
    input: &InputSender,
    shortcuts: &mut Option<MessageStream>,
    changes: &mut DesktopChanges,
    session_root: &str,
) -> anyhow::Result<()> {
    let mut held = false;
    loop {
        tokio::select! {
            biased;
            () = input.closed() => return Ok(()),
            Err(error) = revocation(changes, session_root) => return Err(error),
            event = or_pending(shortcuts.as_mut().map(|stream| stream.next())) => {
                let event = event.context("Shortcut portal disconnected")??;
                let activated = match event.header().member().map(MemberName::as_str) {
                    Some("Activated") => true,
                    Some("Deactivated") => false,
                    _ => continue,
                };
                let (_, action, _, _) = event.body().deserialize::<(OwnedObjectPath, String, u64, Results)>()?;
                if let Some(event) = shortcut_event(&mut held, &action, activated) {
                    input.deliver(event);
                }
            },
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
    stopping: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    if options.paste == LinuxPaste::Manual && options.external_shortcut {
        return x11::run(input, options, requests, stopping).await;
    }
    let (mut portal, mut changes) = tokio::select! {
        biased;
        () = stop_requested(stopping, input) => return Ok(()),
        connected = Portal::connect() => connected?,
    };

    let session_root = portal.session_root();
    let bound = tokio::select! {
        biased;
        () = stop_requested(stopping, input) => None,
        bound = bind(&mut portal, options) => Some(bound),
        Err(revoked) = revocation(&mut changes, &session_root) => Some(Err(revoked)),
    };
    let outcome = match bound {
        Some(Ok(bound)) => match x11::ClipboardWorker::start(input.clone()) {
            Ok(worker) => serve(&portal, bound, changes, worker, input, requests, stopping).await,
            Err(error) => Err(error),
        },
        Some(Err(error)) => Err(error),
        None => Ok(()),
    };
    portal.close().await;
    outcome
}

async fn bind(portal: &mut Portal, options: &DesktopOptions) -> anyhow::Result<Bound> {
    let automatic = match PasteKeys::of(options.paste) {
        Some(keys) => Some((ei::Api::load()?, keys)),
        None => None,
    };
    let (shortcut_session, labels) = if options.external_shortcut {
        (None, ShortcutLabels::new(options))
    } else {
        let (session, labels) = bind_shortcuts(portal, options).await?;
        (Some(session), labels)
    };
    let (sender, clipboard_session) = match automatic {
        Some((libei, keys)) => {
            let (sender, clipboard_session) = start_remote_desktop(portal, libei, keys).await?;
            (Some(sender), clipboard_session)
        },
        None => (None, None),
    };
    Ok(Bound {
        shortcut_session,
        sender,
        clipboard_session,
        labels,
        paste: options.paste,
    })
}

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
                ("preferred_trigger", Value::from(options.shortcut.as_str())),
            ]),
        ),
        (
            CANCEL_ID,
            Options::from([
                ("description", Value::from("Cancel dictation")),
                ("preferred_trigger", Value::from(options.cancel.as_str())),
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
    keys: PasteKeys,
) -> anyhow::Result<(ei::Sender, Option<OwnedObjectPath>)> {
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
    let clipboard_shared = clipboard_requested
        && result
            .remove("clipboard_enabled")
            .and_then(|value| bool::try_from(value).ok())
            .unwrap_or(false);
    let descriptor: zbus::zvariant::OwnedFd = portal
        .proxy(REMOTE)
        .await?
        .call("ConnectToEIS", &(&session, Options::new()))
        .await?;
    Ok((
        ei::Sender::connect(libei, descriptor.into(), keys)?,
        clipboard_shared.then_some(session),
    ))
}

async fn request_clipboard(portal: &Portal, session: &OwnedObjectPath) -> anyhow::Result<bool> {
    let clipboard = portal.proxy(CLIPBOARD).await?;

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
    bound: Bound,
    changes: DesktopChanges,
    worker: x11::ClipboardWorker,
    input: &InputSender,
    requests: Receiver<Insertion>,
    stopping: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    let Bound {
        shortcut_session,
        sender,
        clipboard_session,
        labels,
        paste,
    } = bound;
    let shortcuts = match &shortcut_session {
        Some(session) => Some(session_signals(&portal.connection, SHORTCUTS, session).await?),
        None => None,
    };
    let clipboard = portal.proxy(CLIPBOARD).await?;
    let clipboard_signals = match &clipboard_session {
        Some(session) => Some(session_signals(&portal.connection, CLIPBOARD, session).await?),
        None => None,
    };
    let mut pump = JoinSet::new();
    pump.spawn_local(watch_shortcuts(
        input.clone(),
        shortcuts,
        changes,
        portal.session_root(),
    ));

    tokio::select! {
        biased;
        () = stop_requested(stopping, input) => return Ok(()),
        ready = worker.ready() => ready?,
    }
    input.deliver(labels.into_desktop_ready());
    Insertions::new(sender, clipboard_session, &worker, &clipboard, paste)
        .serve(clipboard_signals, requests, input, stopping)
        .await
}

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

async fn session_signals(
    connection: &Connection,
    interface: &str,
    session: &OwnedObjectPath,
) -> anyhow::Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DESTINATION)?
        .interface(interface)?
        .path(DESKTOP)?
        .arg_path(0, session.as_str())?
        .build();
    Ok(MessageStream::for_match_rule(rule, connection, None).await?)
}

async fn revocation(
    changes: &mut DesktopChanges,
    session_root: &str,
) -> anyhow::Result<Infallible> {
    while let Some(event) = changes.next().await {
        check_revocation(&event?, session_root)?;
    }
    bail!("Portal session monitoring stopped")
}

fn check_revocation(event: &Message, session_root: &str) -> anyhow::Result<()> {
    let header = event.header();
    if header
        .member()
        .is_some_and(|member| member.as_str() == "NameOwnerChanged")
    {
        let (_, previous, _): (String, String, String) = event.body().deserialize()?;
        ensure!(previous.is_empty(), "The desktop portal restarted");
        return Ok(());
    }
    if header
        .path()
        .is_some_and(|closed| closed.as_str().starts_with(session_root))
    {
        bail!("Desktop permissions were revoked");
    }
    Ok(())
}

async fn watch_portal(connection: &Connection) -> anyhow::Result<DesktopChanges> {
    let restarts =
        MessageStream::for_match_rule(owner_changes(DESTINATION)?, connection, None).await?;
    let closed = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DESTINATION)?
        .interface(SESSION)?
        .member("Closed")?
        .build();
    let closed = MessageStream::for_match_rule(closed, connection, None).await?;
    Ok(futures_util::stream::select(closed, restarts))
}

async fn or_pending<F: Future>(future: Option<F>) -> F::Output {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
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
    mod bus;

    use super::*;

    pub(super) fn fixture_session() -> anyhow::Result<OwnedObjectPath> {
        Ok(format!("{DESKTOP}/session/fixture").try_into()?)
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
    fn session_closure_and_portal_restart_stop_desktop_access() -> anyhow::Result<()> {
        let root = format!("{DESKTOP}/session/1_23/");
        let closed = |path: &str| -> anyhow::Result<Message> {
            Ok(Message::signal(path, SESSION, "Closed")?.build(&())?)
        };
        assert!(check_revocation(&closed(&format!("{root}s1_2"))?, &root).is_err());
        assert!(
            check_revocation(&closed(&format!("{DESKTOP}/session/1_234/s1_2"))?, &root).is_ok(),
            "Another connection's session closure revoked access"
        );
        let owner_changed = |previous: &str| {
            Message::signal(
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameOwnerChanged",
            )?
            .build(&(DESTINATION, previous, ":1.8"))
        };
        assert!(check_revocation(&owner_changed("")?, &root).is_ok());
        assert!(check_revocation(&owner_changed(":1.7")?, &root).is_err());
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
