//! The menu bar status item through `tray-icon`. Native callbacks only queue commands; the UI
//! thread runs them and presents each tray state.

use gpui::{App, Global, Task};
use speakeasy_dictation::status::Indicator;
use speakeasy_platform::{Activity, ServiceState};
use tray_icon::{
    BadIcon, Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};

use super::{TrayChanges, TrayCommand, TrayState, icon};

const COMMAND_BACKLOG: usize = 16;

pub(crate) struct Tray {
    _icon: TrayIcon,
    _events: Task<()>,
    _updates: Task<()>,
}

impl Global for Tray {}

/// Writes tray states into the native icon and the menu items that follow them.
struct Presenter {
    icon: TrayIcon,
    status: MenuItem,
    pause: MenuItem,
    toggle: MenuItem,
    cancel: MenuItem,
    drawn: Option<Indicator>,
}

impl Presenter {
    fn present(&mut self, state: &TrayState) {
        self.redraw_icon(state.status.indicator);
        self.status.set_text(&state.status.description);
        best_effort(self.icon.set_tooltip(Some(format!(
            "Speakeasy · {}\nHold {} to dictate",
            state.status.description,
            speakeasy_platform::SHORTCUT
        ))));
        let running = state.service == ServiceState::Running;
        let can_pause = state.interactive && state.service != ServiceState::Pausing;
        let can_toggle = state.interactive && running && state.activity != Activity::Finishing;
        let can_cancel = state.interactive && running && state.activity != Activity::Idle;
        self.pause.set_text(match state.service {
            ServiceState::Running => "Pause dictation",
            ServiceState::Pausing => "Pausing…",
            ServiceState::Paused => "Resume dictation",
        });
        self.pause.set_enabled(can_pause);
        self.toggle.set_text(match state.activity {
            Activity::Dictating => "Finish dictation",
            Activity::Idle | Activity::Finishing => "Start dictation",
        });
        self.toggle.set_enabled(can_toggle);
        self.cancel.set_enabled(can_cancel);
    }

    fn redraw_icon(&mut self, indicator: Indicator) {
        if self.drawn == Some(indicator) {
            return;
        }
        self.drawn = Some(indicator);
        let Ok(image) = draw_icon(indicator) else {
            return;
        };
        best_effort(self.icon.set_icon_templated(Some(image)));
    }
}

pub(crate) fn install(cx: &mut App) -> anyhow::Result<()> {
    let menu = Menu::new();
    let status = MenuItem::new("Loading…", false, None);
    let pause = MenuItem::new("Pause dictation", true, None);
    let toggle = MenuItem::new("Start dictation", false, None);
    let cancel = MenuItem::new("Cancel dictation", false, None);
    let settings = MenuItem::new("Settings…", true, None);
    let quit = MenuItem::new("Quit Speakeasy", true, None);
    menu.append_items(&[
        &status,
        &PredefinedMenuItem::separator(),
        &pause,
        &toggle,
        &cancel,
        &PredefinedMenuItem::separator(),
        &settings,
        &quit,
    ])?;
    let icon = build_icon(menu)?;
    let commands = queue_commands([
        (settings.id().clone(), TrayCommand::Settings),
        (pause.id().clone(), TrayCommand::Pause),
        (toggle.id().clone(), TrayCommand::Toggle),
        (cancel.id().clone(), TrayCommand::Cancel),
        (quit.id().clone(), TrayCommand::Quit),
    ]);
    let events = cx.spawn(async move |cx| {
        while let Ok(command) = commands.recv().await {
            if cx.update(|cx| command.run(cx)).is_err() {
                break;
            }
        }
    });
    let mut presenter = Presenter {
        icon: icon.clone(),
        status,
        pause,
        toggle,
        cancel,
        drawn: None,
    };
    let mut changes = TrayChanges::new(cx);
    let updates = cx.spawn(async move |cx| {
        while let Some(state) = changes.next(cx).await {
            presenter.present(state);
        }
    });
    cx.set_global(Tray {
        _icon: icon,
        _events: events,
        _updates: updates,
    });
    Ok(())
}

fn build_icon(menu: Menu) -> anyhow::Result<TrayIcon> {
    Ok(TrayIconBuilder::new()
        .with_tooltip("Speakeasy")
        .with_menu(Box::new(menu))
        .with_icon_templated(draw_icon(Indicator::Paused)?)
        .build()?)
}

fn draw_icon(indicator: Indicator) -> Result<Icon, BadIcon> {
    Icon::from_rgba(icon::raster(indicator), icon::SIZE, icon::SIZE)
}

/// Installs the native click handlers, which run outside GPUI and can only queue commands.
fn queue_commands<const N: usize>(
    items: [(MenuId, TrayCommand); N],
) -> async_channel::Receiver<TrayCommand> {
    let (queue, commands) = async_channel::bounded(COMMAND_BACKLOG);
    let menu_queue = queue.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some(&(_, command)) = items.iter().find(|(id, _)| *id == event.id) {
            offer(&menu_queue, command);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event| {
        if matches!(
            event,
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        ) {
            offer(&queue, TrayCommand::Settings);
        }
    }));
    commands
}

fn offer(queue: &async_channel::Sender<TrayCommand>, command: TrayCommand) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "Native callbacks cannot wait for the UI; disposal or a full command queue rejects this click"
    )]
    let _ = queue.try_send(command);
}

fn best_effort(update: tray_icon::Result<()>) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A failed cosmetic icon or tooltip update keeps the previous one and a working menu"
    )]
    let _ = update;
}
