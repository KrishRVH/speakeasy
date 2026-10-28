//! The Windows and macOS tray through `tray-icon`. Native callbacks only queue commands; the UI
//! thread runs them and presents each tray state.

use gpui::{App, Global, Task};
use speakeasy_platform::{TrayItem, TrayMenu};
use tray_icon::{
    BadIcon, Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};

use super::{
    TrayChanges, TrayCommand, TrayState,
    icon::{self, IconStyle},
};
use crate::{shell::Services, status::Indicator, theme::Theme};

const ICON_STYLE: IconStyle = if cfg!(target_os = "macos") {
    IconStyle::Template
} else {
    IconStyle::Color
};
const COMMAND_BACKLOG: usize = 16;

pub(crate) struct Tray {
    _events: Task<()>,
    _updates: Task<()>,
}

impl Global for Tray {}

/// Owns the native icon and the stateful menu items, and writes each tray state into them.
struct Presenter {
    icon: TrayIcon,
    status: MenuItem,
    pause: MenuItem,
    toggle: MenuItem,
    cancel: MenuItem,
    drawn: Option<(Indicator, Theme)>,
}

impl Presenter {
    fn present(&mut self, state: &TrayState) {
        best_effort(self.redraw_icon(state.status.indicator, state.theme));
        self.status.set_text(&state.status.description);
        best_effort(self.icon.set_tooltip(Some(format!(
            "Speakeasy · {}\nHold {} to dictate",
            state.status.description,
            speakeasy_platform::SHORTCUT
        ))));
        let TrayMenu {
            pause,
            toggle,
            cancel,
        } = state.menu;
        for (entry, TrayItem { label, enabled }) in [
            (&self.pause, pause),
            (&self.toggle, toggle),
            (&self.cancel, cancel),
        ] {
            entry.set_text(label);
            entry.set_enabled(enabled);
        }
    }

    fn redraw_icon(&mut self, indicator: Indicator, theme: Theme) -> anyhow::Result<()> {
        if self.drawn == Some((indicator, theme)) {
            return Ok(());
        }
        self.drawn = Some((indicator, theme));
        let image = draw_icon(indicator, theme)?;
        #[cfg(target_os = "macos")]
        self.icon.set_icon_templated(Some(image))?;
        #[cfg(target_os = "windows")]
        self.icon.set_icon(Some(image))?;
        Ok(())
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
    let icon = build_icon(menu, cx.global::<Services>().config.theme)?;
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
        icon,
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
        _events: events,
        _updates: updates,
    });
    Ok(())
}

fn build_icon(menu: Menu, theme: Theme) -> anyhow::Result<TrayIcon> {
    let image = draw_icon(Indicator::Paused, theme)?;
    let builder = TrayIconBuilder::new()
        .with_tooltip("Speakeasy")
        .with_menu(Box::new(menu));
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_templated(image);
    #[cfg(target_os = "windows")]
    let builder = builder.with_icon(image);
    Ok(builder.build()?)
}

fn draw_icon(indicator: Indicator, theme: Theme) -> Result<Icon, BadIcon> {
    Icon::from_rgba(
        icon::raster(indicator, theme, ICON_STYLE),
        icon::SIZE,
        icon::SIZE,
    )
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

fn best_effort<E>(update: Result<(), E>) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A failed cosmetic icon or tooltip update keeps the previous one and a working menu"
    )]
    let _ = update;
}
