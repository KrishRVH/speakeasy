use anyhow::Context;
use futures_util::StreamExt;
use std::{collections::HashMap, thread};
use zbus::{
    Connection, MatchRule, MessageStream,
    object_server::SignalEmitter,
    zvariant::{OwnedObjectPath, Value},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Tray host observations and commands routed to the UI owner.
pub enum TrayAction {
    /// Whether a compatible tray host is present.
    Available(bool),
    /// Open Settings.
    Settings,
    /// Pause or resume dictation.
    Pause,
    /// Start or finish hands-free capture.
    Toggle,
    /// Cancel the active recording or recognition.
    Cancel,
    /// Request orderly application shutdown.
    Quit,
}
/// Immutable icon and menu snapshot sent to the native tray owner.
#[expect(
    clippy::struct_excessive_bools,
    reason = "These independent menu capabilities are an immutable presentation snapshot, not session state"
)]
pub struct TrayPresentation {
    /// 32-by-32 straight-alpha icon pixels in RGBA order.
    pub rgba: Vec<u8>,
    /// Accessible description of the current dictation phase.
    pub description: String,
    /// A runtime owns the dictation resources.
    pub running: bool,
    /// Native resources are retiring.
    pub pausing: bool,
    /// Capture is starting or recording.
    pub capturing: bool,
    /// A recording, recognition, or insertion is in progress.
    pub active: bool,
    /// Dictation can be resumed.
    pub enabled: bool,
    /// Capture can start immediately.
    pub ready: bool,
}
/// Owns the D-Bus tray thread and closes its channels before joining.
pub struct NativeTray {
    updates: async_channel::Sender<TrayPresentation>,
    stop: async_channel::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}
impl NativeTray {
    /// Start tray registration without microphone, hook, or clipboard access.
    ///
    /// # Errors
    /// Returns a thread startup failure; absent tray hosts arrive as availability events.
    pub fn start() -> anyhow::Result<(Self, async_channel::Receiver<TrayAction>)> {
        let (updates, incoming) = async_channel::bounded(1);
        let (events, outgoing) = async_channel::bounded(16);
        let (stop, stopping) = async_channel::bounded(1);
        let thread = thread::Builder::new()
            .name("linux-tray".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    #[expect(clippy::let_underscore_must_use, reason = "Tray failure is reported by the following availability event; the application remains usable without a tray host")]
                    let _ = runtime.block_on(serve(incoming, &events, &stopping));
                }
                publish_availability(&events, false);
            })?;
        Ok((
            Self {
                updates,
                stop,
                thread: Some(thread),
            },
            outgoing,
        ))
    }
    /// Obtain the snapshot lane; a full lane coalesces updates at the caller.
    #[must_use]
    pub fn updater(&self) -> async_channel::Sender<TrayPresentation> {
        self.updates.clone()
    }
}
impl Drop for NativeTray {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The stop lane is idempotent; full already requests stop and closed means the tray thread ended"
        )]
        let _ = self.stop.try_send(());
        self.updates.close();
        if let Some(thread) = self.thread.take() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Tray channels are closed before joining; a tray-worker panic must not unwind this destructor"
            )]
            let _ = thread.join();
        }
    }
}
struct Item {
    title: String,
    icon: Vec<(i32, i32, Vec<u8>)>,
    events: async_channel::Sender<TrayAction>,
}
type IconPixmaps = Vec<(i32, i32, Vec<u8>)>;
type ToolTip<'a> = (&'a str, &'a IconPixmaps, &'a str, &'a str);
#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn category(&self) -> &'static str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn id(&self) -> &'static str {
        "speakeasy"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        &self.title
    }
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn status(&self) -> &'static str {
        "Active"
    }
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn window_id(&self) -> u32 {
        0
    }
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn icon_name(&self) -> &'static str {
        ""
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> &Vec<(i32, i32, Vec<u8>)> {
        &self.icon
    }
    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip<'_> {
        ("", &self.icon, "Speakeasy", &self.title)
    }
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn menu(&self) -> OwnedObjectPath {
        zbus::zvariant::ObjectPath::from_static_str_unchecked("/MenuBar").into()
    }
    fn activate(&self, x: i32, y: i32) {
        let _ = (x, y);
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The bounded tray UI lane never blocks D-Bus callbacks; a busy or closed UI cannot accept this command"
        )]
        let _ = self.events.try_send(TrayAction::Settings);
    }
    fn secondary_activate(&self, x: i32, y: i32) {
        let _ = (x, y);
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The bounded tray UI lane never blocks D-Bus callbacks; a busy or closed UI cannot accept this command"
        )]
        let _ = self.events.try_send(TrayAction::Toggle);
    }
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn context_menu(&self, x: i32, y: i32) {
        let _ = (x, y);
    }
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn scroll(&self, delta: i32, orientation: &str) {
        let _ = (delta, orientation);
    }
    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}
struct Menu {
    items: Vec<(i32, String, bool)>,
    revision: u32,
    events: async_channel::Sender<TrayAction>,
}
type Properties = HashMap<String, Value<'static>>;
type Layout = (i32, Properties, Vec<Value<'static>>);
fn properties(label: &str, enabled: bool) -> Properties {
    HashMap::from([
        ("label".into(), Value::from(label.to_owned())),
        ("enabled".into(), Value::from(enabled)),
        ("visible".into(), Value::from(true)),
    ])
}
#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn text_direction(&self) -> &'static str {
        "ltr"
    }
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn status(&self) -> &'static str {
        "normal"
    }
    #[zbus(property)]
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }
    fn get_layout(&self, parent: i32, depth: i32, property_names: Vec<String>) -> (u32, Layout) {
        let _ = (property_names,);
        let children = if parent == 0 && depth != 0 {
            self.items
                .iter()
                .map(|(id, label, enabled)| {
                    Value::from((
                        *id,
                        properties(label, *enabled),
                        Vec::<Value<'static>>::new(),
                    ))
                })
                .collect()
        } else {
            Vec::new()
        };
        let root = if parent == 0 {
            HashMap::from([("children-display".into(), Value::from("submenu"))])
        } else {
            self.items
                .iter()
                .find(|item| item.0 == parent)
                .map_or_default(|(_, label, enabled)| properties(label, *enabled))
        };
        (self.revision, (parent, root, children))
    }
    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, Properties)> {
        let _ = (property_names,);
        self.items
            .iter()
            .filter(|item| ids.is_empty() || ids.contains(&item.0))
            .map(|(id, label, enabled)| (*id, properties(label, *enabled)))
            .collect()
    }
    fn get_property(&self, id: i32, name: &str) -> zbus::fdo::Result<Value<'static>> {
        self.items
            .iter()
            .find(|item| item.0 == id)
            .and_then(|(_, label, enabled)| properties(label, *enabled).remove(name))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Unknown menu property".into()))
    }
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn about_to_show(&self, id: i32) -> bool {
        let _ = (id,);
        false
    }
    #[expect(
        clippy::unused_self,
        reason = "D-Bus dispatch requires an instance method even for this constant protocol response"
    )]
    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        let _ = (ids,);
        (Vec::new(), Vec::new())
    }
    fn event(&self, id: i32, event: &str, data: Value<'_>, timestamp: u32) {
        let _ = (data, timestamp);
        if event != "clicked" || !self.items.iter().any(|item| item.0 == id && item.2) {
            return;
        }
        let action = match id {
            1 => TrayAction::Pause,
            2 => TrayAction::Toggle,
            3 => TrayAction::Cancel,
            4 => TrayAction::Settings,
            5 => TrayAction::Quit,
            _ => return,
        };
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The bounded tray UI lane never blocks D-Bus callbacks; a busy or closed UI cannot accept this command"
        )]
        let _ = self.events.try_send(action);
    }
    fn event_group(&self, events: Vec<(i32, String, Value<'_>, u32)>) -> Vec<i32> {
        for (id, event, data, timestamp) in events {
            self.event(id, &event, data, timestamp);
        }
        Vec::new()
    }
    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}
fn publish_availability(events: &async_channel::Sender<TrayAction>, available: bool) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Tray availability is an optional bounded UI notification; closure means the UI owner is gone"
    )]
    let _ = events.try_send(TrayAction::Available(available));
}
async fn register(connection: &Connection) -> bool {
    let result = tokio::time::timeout(std::time::Duration::from_millis(500), async {
        let watcher = zbus::Proxy::new(
            connection,
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
        )
        .await?;
        let name = connection
            .unique_name()
            .context("Tray connection has no name")?
            .as_str();
        watcher
            .call::<_, _, ()>("RegisterStatusNotifierItem", &(name,))
            .await?;
        let available = watcher
            .get_property::<bool>("IsStatusNotifierHostRegistered")
            .await?;
        Ok::<_, anyhow::Error>(available)
    })
    .await;
    matches!(result, Ok(Ok(true)))
}
async fn serve(
    updates: async_channel::Receiver<TrayPresentation>,
    events: &async_channel::Sender<TrayAction>,
    stop: &async_channel::Receiver<()>,
) -> anyhow::Result<()> {
    let connection = tokio::select! { _ = stop.recv() => return Ok(()), connection = Connection::session() => connection? };
    connection
        .object_server()
        .at(
            "/StatusNotifierItem",
            Item {
                title: "Speakeasy".into(),
                icon: Vec::new(),
                events: events.clone(),
            },
        )
        .await?;
    connection
        .object_server()
        .at(
            "/MenuBar",
            Menu {
                items: Vec::new(),
                revision: 0,
                events: events.clone(),
            },
        )
        .await?;
    let item = connection
        .object_server()
        .interface::<_, Item>("/StatusNotifierItem")
        .await?;
    let menu = connection
        .object_server()
        .interface::<_, Menu>("/MenuBar")
        .await?;
    let watcher_rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.kde.StatusNotifierWatcher")?
        .interface("org.kde.StatusNotifierWatcher")?
        .path("/StatusNotifierWatcher")?
        .build();
    let ownership_rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .add_arg("org.kde.StatusNotifierWatcher")?
        .build();
    let mut changes = futures_util::stream::select(
        MessageStream::for_match_rule(watcher_rule, &connection, Some(16)).await?,
        MessageStream::for_match_rule(ownership_rule, &connection, Some(16)).await?,
    );
    let mut available = register(&connection).await;
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Tray availability is an optional bounded UI notification; closure means the UI owner is gone"
    )]
    let _ = events.try_send(TrayAction::Available(available));
    loop {
        tokio::select! {
            biased;
            _ = stop.recv() => break,
            signal = changes.next() => {
                let signal = signal.context("Tray connection closed")??;
                let header = signal.header();
                let watcher = header.interface().is_some_and(|name| name.as_str() == "org.kde.StatusNotifierWatcher") && header.member().is_some_and(|name| matches!(name.as_str(), "StatusNotifierHostRegistered" | "StatusNotifierHostUnregistered"));
                let ownership = header.member().is_some_and(|name| name.as_str() == "NameOwnerChanged") && signal.body().deserialize::<(String, String, String)>().is_ok_and(|(name, _, _)| name == "org.kde.StatusNotifierWatcher");
                if watcher || ownership {
                    let now = register(&connection).await;
                    if now != available {
                        available = now;
                        publish_availability(events, now);
                    }
                }
            }
            presentation = updates.recv() => {
                let Ok(presentation) = presentation else { break; };
                {
                    let mut item = item.get_mut().await;
                    item.title.clone_from(&presentation.description);
                    item.icon = vec![(32, 32, presentation.rgba.as_chunks::<4>().0.iter().flat_map(|pixel| [pixel[3], pixel[0], pixel[1], pixel[2]]).collect())];
                }
                Item::new_icon(item.signal_emitter()).await?;
                Item::new_tool_tip(item.signal_emitter()).await?;
                let revision = {
                    let mut menu = menu.get_mut().await;
                    menu.revision = menu.revision.wrapping_add(1);
                    menu.items = vec![
                    (6, presentation.description, false),
                    (1, if presentation.pausing { "Pausing…" } else if presentation.running { "Pause dictation" } else { "Resume dictation" }.into(), !presentation.pausing && presentation.enabled),
                    (2, if presentation.capturing { "Finish dictation" } else { "Start dictation" }.into(), presentation.running && !presentation.pausing && (!presentation.active || presentation.capturing) && presentation.enabled && presentation.ready),
                    (3, "Cancel dictation".into(), presentation.running && presentation.active && presentation.enabled),
                    (4, "Settings…".into(), true), (5, "Quit Speakeasy".into(), true),
                    ];
                    menu.revision
                };
                Menu::layout_updated(menu.signal_emitter(), revision, 0).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menu_ignores_disabled_unknown_and_non_click_events() -> anyhow::Result<()> {
        let (events, received) = async_channel::bounded(4);
        let menu = Menu {
            items: vec![(1, "Pause".into(), false), (4, "Settings".into(), true)],
            revision: 1,
            events,
        };
        menu.event(1, "clicked", Value::from(0_i32), 0);
        menu.event(99, "clicked", Value::from(0_i32), 0);
        menu.event(4, "hovered", Value::from(0_i32), 0);
        assert!(received.try_recv().is_err());
        menu.event(4, "clicked", Value::from(0_i32), 0);
        assert_eq!(received.try_recv()?, TrayAction::Settings);
        assert!(menu.get_property(99, "enabled").is_err());
        let (_, (_, _, children)) = menu.get_layout(0, -1, Vec::new());
        assert_eq!(children.len(), 2);
        Ok(())
    }
}
