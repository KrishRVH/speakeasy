use anyhow::Context;
use futures_util::StreamExt;
use std::{collections::HashMap, thread};
use zbus::{
    Connection, MatchRule, MessageStream,
    object_server::SignalEmitter,
    zvariant::{OwnedObjectPath, Value},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Available(bool),
    Settings,
    Pause,
    Toggle,
    Cancel,
    Quit,
}
pub struct TrayPresentation {
    pub rgba: Vec<u8>,
    pub description: String,
    pub running: bool,
    pub pausing: bool,
    pub capturing: bool,
    pub active: bool,
    pub enabled: bool,
    pub ready: bool,
}
pub struct NativeTray {
    updates: async_channel::Sender<TrayPresentation>,
    stop: async_channel::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}
impl NativeTray {
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
                    let _ = runtime.block_on(serve(incoming, &events, &stopping));
                }
                let _ = events.try_send(TrayAction::Available(false));
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
    pub fn updater(&self) -> async_channel::Sender<TrayPresentation> {
        self.updates.clone()
    }
}
impl Drop for NativeTray {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
        self.updates.close();
        if let Some(thread) = self.thread.take() {
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
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "speakeasy"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        &self.title
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn window_id(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
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
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        zbus::zvariant::ObjectPath::from_static_str_unchecked("/MenuBar").into()
    }
    fn activate(&self, _x: i32, _y: i32) {
        let _ = self.events.try_send(TrayAction::Settings);
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {
        let _ = self.events.try_send(TrayAction::Toggle);
    }
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: &str) {}
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
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }
    fn get_layout(&self, parent: i32, depth: i32, _property_names: Vec<String>) -> (u32, Layout) {
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
                .map(|(_, label, enabled)| properties(label, *enabled))
                .unwrap_or_default()
        };
        (self.revision, (parent, root, children))
    }
    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _property_names: Vec<String>,
    ) -> Vec<(i32, Properties)> {
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
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }
    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }
    fn event(&self, id: i32, event: &str, _data: Value<'_>, _timestamp: u32) {
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
                    if now != available { available = now; let _ = events.try_send(TrayAction::Available(now)); }
                }
            }
            presentation = updates.recv() => {
                let Ok(presentation) = presentation else { break; };
                {
                    let mut item = item.get_mut().await;
                    item.title = presentation.description.clone();
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
