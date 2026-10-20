//! The `StatusNotifierItem` tray icon and its dbusmenu, served on an owned D-Bus thread. Menu
//! clicks reach the UI only for enabled entries, and tray failure leaves the app usable.

use std::{collections::HashMap, convert::Infallible, time::Duration};

use anyhow::Context;
use async_channel::{Receiver, Sender};
use futures_util::{StreamExt, stream::Select};
use zbus::{
    Connection, MatchRule, Message, MessageStream,
    object_server::{InterfaceRef, SignalEmitter},
    zvariant::{ObjectPath, OwnedObjectPath, Value},
};

use super::owner_changes;
use crate::{Activity, OwnedThread, ServiceState};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const ICON_SIZE: i32 = 32;
/// dbusmenu ID of the root menu; entries must not use it.
const ROOT_ID: i32 = 0;
const REGISTRATION_TIMEOUT: Duration = Duration::from_millis(500);
const ACTION_BACKLOG: usize = 16;
const SIGNAL_BACKLOG: usize = 16;

/// Tray host observations and commands routed to the UI owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
pub struct TrayPresentation {
    /// 32-by-32 straight-alpha icon pixels in RGBA order.
    pub rgba: Vec<u8>,
    /// Accessible description of the current dictation phase.
    pub description: String,
    /// Whether dictation is running, pausing, or paused.
    pub service: ServiceState,
    /// What dictation is doing.
    pub activity: Activity,
    /// Whether the pause, dictation, and cancel entries may act.
    pub interactive: bool,
    /// Whether desktop input is ready, so capture can start immediately.
    pub desktop_ready: bool,
}

/// Owns the D-Bus tray thread and closes its channels before joining.
pub struct NativeTray {
    _thread: OwnedThread,
    updates: Sender<TrayPresentation>,
    stop: Sender<Infallible>,
}

impl NativeTray {
    /// Starts tray registration without microphone, hook, or clipboard access.
    ///
    /// # Errors
    /// Returns a thread startup failure; absent tray hosts arrive as availability events.
    pub fn start() -> anyhow::Result<(Self, Receiver<TrayAction>)> {
        let (updates, incoming) = async_channel::bounded(1);
        let (actions, received) = async_channel::bounded(ACTION_BACKLOG);
        let (stop, stop_requests) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("tray-service", move || {
            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Tray failure is reported by the following availability event; the application remains usable without a tray host"
                )]
                let _ = runtime.block_on(serve(incoming, &actions, &stop_requests));
            }
            offer(&actions, TrayAction::Available(false));
        })?;
        Ok((
            Self {
                _thread: thread,
                updates,
                stop,
            },
            received,
        ))
    }

    /// Returns the snapshot lane; a full lane coalesces updates at the caller.
    #[must_use]
    pub fn updater(&self) -> Sender<TrayPresentation> {
        self.updates.clone()
    }
}

impl Drop for NativeTray {
    fn drop(&mut self) {
        self.stop.close();
        self.updates.close();
    }
}

type IconPixmaps = Vec<(i32, i32, Vec<u8>)>;
type ToolTip<'a> = (&'a str, &'a IconPixmaps, &'a str, &'a str);

struct Item {
    title: String,
    icon: IconPixmaps,
    events: Sender<TrayAction>,
}

impl Item {
    fn present(&mut self, presentation: &TrayPresentation) {
        self.title.clone_from(&presentation.description);
        self.icon = vec![(ICON_SIZE, ICON_SIZE, argb(&presentation.rgba))];
    }
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn category(&self) -> &'static str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn id(&self) -> &'static str {
        "speakeasy"
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        &self.title
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn status(&self) -> &'static str {
        "Active"
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn window_id(&self) -> u32 {
        0
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn icon_name(&self) -> &'static str {
        ""
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> &IconPixmaps {
        &self.icon
    }

    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip<'_> {
        ("", &self.icon, "Speakeasy", &self.title)
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn menu(&self) -> OwnedObjectPath {
        ObjectPath::from_static_str_unchecked(MENU_PATH).into()
    }

    fn activate(&self, x: i32, y: i32) {
        // D-Bus introspection publishes argument names, so unused ones are discarded, not renamed.
        let _ = (x, y);
        offer(&self.events, TrayAction::Settings);
    }

    fn secondary_activate(&self, x: i32, y: i32) {
        let _ = (x, y);
        offer(&self.events, TrayAction::Toggle);
    }

    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn context_menu(&self, x: i32, y: i32) {
        let _ = (x, y);
    }

    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn scroll(&self, delta: i32, orientation: &str) {
        let _ = (delta, orientation);
    }

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Menu entries; each discriminant is the entry's dbusmenu item ID, which must differ from
/// `ROOT_ID`.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
enum MenuEntry {
    Pause = 1,
    Toggle = 2,
    Cancel = 3,
    Settings = 4,
    Quit = 5,
    Status = 6,
}

impl MenuEntry {
    const fn id(self) -> i32 {
        self as i32
    }

    const fn action(self) -> Option<TrayAction> {
        match self {
            Self::Pause => Some(TrayAction::Pause),
            Self::Toggle => Some(TrayAction::Toggle),
            Self::Cancel => Some(TrayAction::Cancel),
            Self::Settings => Some(TrayAction::Settings),
            Self::Quit => Some(TrayAction::Quit),
            Self::Status => None,
        }
    }
}

type Properties = HashMap<String, Value<'static>>;
type Layout = (i32, Properties, Vec<Value<'static>>);

struct MenuItem {
    entry: MenuEntry,
    label: String,
    enabled: bool,
}

impl MenuItem {
    fn properties(&self) -> Properties {
        HashMap::from([
            ("label".into(), Value::from(self.label.clone())),
            ("enabled".into(), Value::from(self.enabled)),
            ("visible".into(), Value::from(true)),
        ])
    }
}

struct Menu {
    items: Vec<MenuItem>,
    revision: u32,
    events: Sender<TrayAction>,
}

impl Menu {
    fn item(&self, id: i32) -> Option<&MenuItem> {
        self.items.iter().find(|item| item.entry.id() == id)
    }

    fn present(&mut self, presentation: TrayPresentation) -> u32 {
        self.revision = self.revision.wrapping_add(1);
        self.items = menu_items(presentation);
        self.revision
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn text_direction(&self) -> &'static str {
        "ltr"
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn status(&self) -> &'static str {
        "normal"
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_layout(&self, parent: i32, depth: i32, property_names: Vec<String>) -> (u32, Layout) {
        let _ = property_names;
        let children = if parent == ROOT_ID && depth != 0 {
            self.items
                .iter()
                .map(|item| {
                    Value::from((
                        item.entry.id(),
                        item.properties(),
                        Vec::<Value<'static>>::new(),
                    ))
                })
                .collect()
        } else {
            Vec::new()
        };
        let root = if parent == ROOT_ID {
            HashMap::from([("children-display".into(), Value::from("submenu"))])
        } else {
            self.item(parent).map_or_default(MenuItem::properties)
        };
        (self.revision, (parent, root, children))
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, Properties)> {
        let _ = property_names;
        self.items
            .iter()
            .filter(|item| ids.is_empty() || ids.contains(&item.entry.id()))
            .map(|item| (item.entry.id(), item.properties()))
            .collect()
    }

    fn get_property(&self, id: i32, name: &str) -> zbus::fdo::Result<Value<'static>> {
        self.item(id)
            .and_then(|item| item.properties().remove(name))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Unknown menu property".into()))
    }

    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn about_to_show(&self, id: i32) -> bool {
        let _ = id;
        false
    }

    #[expect(clippy::unused_self, reason = "zbus requires a receiver")]
    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        let _ = ids;
        (Vec::new(), Vec::new())
    }

    fn event(&self, id: i32, event: &str, data: Value<'_>, timestamp: u32) {
        let _ = (data, timestamp);
        if event != "clicked" {
            return;
        }
        if let Some(action) = self
            .item(id)
            .filter(|item| item.enabled)
            .and_then(|item| item.entry.action())
        {
            offer(&self.events, action);
        }
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

fn menu_items(presentation: TrayPresentation) -> Vec<MenuItem> {
    let TrayPresentation {
        description,
        service,
        activity,
        interactive,
        desktop_ready,
        ..
    } = presentation;
    let running = service == ServiceState::Running;
    let pause = match service {
        ServiceState::Running => "Pause dictation",
        ServiceState::Pausing => "Pausing…",
        ServiceState::Paused => "Resume dictation",
    };
    let toggle = match activity {
        Activity::Dictating => "Finish dictation",
        Activity::Idle | Activity::Finishing => "Start dictation",
    };
    vec![
        MenuItem {
            entry: MenuEntry::Status,
            label: description,
            enabled: false,
        },
        MenuItem {
            entry: MenuEntry::Pause,
            label: pause.into(),
            enabled: service != ServiceState::Pausing && interactive,
        },
        MenuItem {
            entry: MenuEntry::Toggle,
            label: toggle.into(),
            enabled: running && activity != Activity::Finishing && interactive && desktop_ready,
        },
        MenuItem {
            entry: MenuEntry::Cancel,
            label: "Cancel dictation".into(),
            enabled: running && activity != Activity::Idle && interactive,
        },
        MenuItem {
            entry: MenuEntry::Settings,
            label: "Settings…".into(),
            enabled: true,
        },
        MenuItem {
            entry: MenuEntry::Quit,
            label: "Quit Speakeasy".into(),
            enabled: true,
        },
    ]
}

/// Reorders RGBA pixels into the ARGB32 network byte order `StatusNotifierItem` pixmaps use.
fn argb(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[red, green, blue, alpha]| [alpha, red, green, blue])
        .collect()
}

/// Queues `action` for the UI without ever blocking a D-Bus callback.
fn offer(events: &Sender<TrayAction>, action: TrayAction) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Tray actions are best-effort notifications; a busy or closed UI owner cannot accept them"
    )]
    let _ = events.try_send(action);
}

async fn serve(
    updates: Receiver<TrayPresentation>,
    events: &Sender<TrayAction>,
    stop: &Receiver<Infallible>,
) -> anyhow::Result<()> {
    let connection = tokio::select! {
        _ = stop.recv() => return Ok(()),
        connection = Connection::session() => connection?,
    };
    let item = Item {
        title: "Speakeasy".into(),
        icon: Vec::new(),
        events: events.clone(),
    };
    let menu = Menu {
        items: Vec::new(),
        revision: 0,
        events: events.clone(),
    };
    let objects = connection.object_server();
    objects.at(ITEM_PATH, item).await?;
    objects.at(MENU_PATH, menu).await?;
    let item = objects.interface::<_, Item>(ITEM_PATH).await?;
    let menu = objects.interface::<_, Menu>(MENU_PATH).await?;
    let mut host_changes = host_changes(&connection).await?;
    let mut available = register(&connection).await;
    offer(events, TrayAction::Available(available));
    loop {
        tokio::select! {
            biased;
            _ = stop.recv() => break,
            signal = host_changes.next() => {
                if is_host_change(&signal.context("Tray connection closed")??) {
                    available = refresh_availability(&connection, events, available).await;
                }
            },
            presentation = updates.recv() => match presentation {
                Ok(presentation) => present(&item, &menu, presentation).await?,
                Err(_) => break,
            },
        }
    }
    Ok(())
}

async fn host_changes(
    connection: &Connection,
) -> anyhow::Result<Select<MessageStream, MessageStream>> {
    let watcher = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(WATCHER)?
        .interface(WATCHER)?
        .path(WATCHER_PATH)?
        .build();
    Ok(futures_util::stream::select(
        MessageStream::for_match_rule(watcher, connection, Some(SIGNAL_BACKLOG)).await?,
        MessageStream::for_match_rule(owner_changes(WATCHER)?, connection, Some(SIGNAL_BACKLOG))
            .await?,
    ))
}

/// Whether `signal` reports a tray host or the watcher coming or going, not an item registration.
/// The match rules already limit each stream to the watcher interface or its owner changes.
fn is_host_change(signal: &Message) -> bool {
    signal.header().member().is_some_and(|member| {
        matches!(
            member.as_str(),
            "StatusNotifierHostRegistered" | "StatusNotifierHostUnregistered" | "NameOwnerChanged"
        )
    })
}

/// Registers the item with the watcher; returns whether a tray host can show it.
async fn register(connection: &Connection) -> bool {
    let registration = async {
        let watcher = zbus::Proxy::new(connection, WATCHER, WATCHER_PATH, WATCHER).await?;
        let name = connection
            .unique_name()
            .context("Tray connection has no name")?
            .as_str();
        watcher
            .call::<_, _, ()>("RegisterStatusNotifierItem", &(name,))
            .await?;
        Ok::<_, anyhow::Error>(
            watcher
                .get_property::<bool>("IsStatusNotifierHostRegistered")
                .await?,
        )
    };
    matches!(
        tokio::time::timeout(REGISTRATION_TIMEOUT, registration).await,
        Ok(Ok(true))
    )
}

async fn refresh_availability(
    connection: &Connection,
    events: &Sender<TrayAction>,
    available: bool,
) -> bool {
    let now = register(connection).await;
    if now != available {
        offer(events, TrayAction::Available(now));
    }
    now
}

async fn present(
    item: &InterfaceRef<Item>,
    menu: &InterfaceRef<Menu>,
    presentation: TrayPresentation,
) -> zbus::Result<()> {
    item.get_mut().await.present(&presentation);
    Item::new_icon(item.signal_emitter()).await?;
    Item::new_tool_tip(item.signal_emitter()).await?;
    let revision = menu.get_mut().await.present(presentation);
    Menu::layout_updated(menu.signal_emitter(), revision, ROOT_ID).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ignores_disabled_unknown_and_non_click_events() -> anyhow::Result<()> {
        let (events, received) = async_channel::bounded(4);
        let menu = Menu {
            items: vec![
                MenuItem {
                    entry: MenuEntry::Pause,
                    label: "Pause".into(),
                    enabled: false,
                },
                MenuItem {
                    entry: MenuEntry::Settings,
                    label: "Settings".into(),
                    enabled: true,
                },
            ],
            revision: 1,
            events,
        };
        menu.event(MenuEntry::Pause.id(), "clicked", Value::from(0_i32), 0);
        menu.event(99, "clicked", Value::from(0_i32), 0);
        menu.event(MenuEntry::Settings.id(), "hovered", Value::from(0_i32), 0);
        assert!(received.try_recv().is_err());
        menu.event(MenuEntry::Settings.id(), "clicked", Value::from(0_i32), 0);
        assert_eq!(received.try_recv()?, TrayAction::Settings);
        assert!(menu.get_property(99, "enabled").is_err());
        let (_, (_, _, children)) = menu.get_layout(ROOT_ID, -1, Vec::new());
        assert_eq!(children.len(), 2);
        Ok(())
    }
}
