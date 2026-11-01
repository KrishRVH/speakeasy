use gpui::{App, Global, Task};
use speakeasy_platform::{Scope, TrayItem, TrayMenu};
use tray_icon::{
    BadIcon, Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu},
};

use super::{
    TrayChanges, TrayCommand, TrayState,
    icon::{self, IconStyle},
};
use crate::{
    shell::{LastShow, Services},
    status::Indicator,
    theme::Theme,
    transcript::style::Style,
};

const ICON_STYLE: IconStyle = if cfg!(target_os = "macos") {
    IconStyle::Template
} else {
    IconStyle::Color
};
const COMMAND_BACKLOG: usize = 16;
const PRESET_ID: &str = "speakeasy-preset:";

pub(crate) struct Tray {
    _events: Task<()>,
    _updates: Task<()>,
}

impl Global for Tray {}

struct Presenter {
    icon: TrayIcon,
    status: MenuItem,
    pause: MenuItem,
    toggle: MenuItem,
    cancel: MenuItem,
    paste_last: MenuItem,
    styles: Vec<(Style, CheckMenuItem)>,
    presets: Submenu,
    preset_items: Vec<(String, CheckMenuItem)>,
    show_screen: MenuItem,
    show_area: MenuItem,
    copy_last_show: MenuItem,
    open_last_show: MenuItem,
    drawn: Option<(Indicator, Theme)>,
}

impl Presenter {
    fn present(&mut self, state: &TrayState) {
        best_effort(self.redraw_icon(state.status.indicator, state.theme));
        self.status.set_text(&state.status.description);
        best_effort(self.icon.set_tooltip(Some(format!(
            "Speakeasy · {}\nHold {} to dictate",
            state.status.description, state.shortcut
        ))));
        let TrayMenu {
            pause,
            toggle,
            cancel,
            paste_last,
            show_screen,
            show_area,
        } = state.menu;
        for (entry, TrayItem { label, enabled }) in [
            (&self.pause, pause),
            (&self.toggle, toggle),
            (&self.cancel, cancel),
            (&self.paste_last, paste_last),
            (&self.show_screen, show_screen),
            (&self.show_area, show_area),
        ] {
            entry.set_text(label);
            entry.set_enabled(enabled);
        }
        self.copy_last_show.set_enabled(state.last_show);
        self.open_last_show.set_enabled(state.last_show);
        for (style, item) in &self.styles {
            item.set_checked(*style == state.style);
        }
        best_effort(self.present_presets(state));
    }

    fn present_presets(&mut self, state: &TrayState) -> anyhow::Result<()> {
        if !self
            .preset_items
            .iter()
            .map(|(name, _)| name)
            .eq(state.presets.iter())
        {
            for (_, item) in &self.preset_items {
                self.presets.remove(item)?;
            }
            self.preset_items.clear();
            for name in &state.presets {
                let item =
                    CheckMenuItem::with_id(format!("{PRESET_ID}{name}"), name, true, false, None);
                self.presets.append(&item)?;
                self.preset_items.push((name.clone(), item));
            }
        }
        self.presets.set_enabled(!self.preset_items.is_empty());
        for (name, item) in &self.preset_items {
            item.set_checked(state.preset.as_ref() == Some(name));
            item.set_enabled(
                state.menu.toggle.enabled && state.menu.toggle.label == "Start dictation",
            );
        }
        Ok(())
    }

    fn redraw_icon(&mut self, indicator: Indicator, theme: Theme) -> anyhow::Result<()> {
        if self.drawn == Some((indicator, theme)) {
            return Ok(());
        }
        let image = draw_icon(indicator, theme)?;
        #[cfg(target_os = "macos")]
        self.icon.set_icon_templated(Some(image))?;
        #[cfg(target_os = "windows")]
        self.icon.set_icon(Some(image))?;
        self.drawn = Some((indicator, theme));
        Ok(())
    }
}

pub(crate) fn install(cx: &mut App) -> anyhow::Result<()> {
    let menu = Menu::new();
    let status = MenuItem::new("Loading…", false, None);
    let pause = MenuItem::new("Pause dictation", true, None);
    let toggle = MenuItem::new("Start dictation", false, None);
    let cancel = MenuItem::new("Cancel dictation", false, None);
    let paste_last = MenuItem::new("Paste last dictation", false, None);
    let style_menu = Submenu::new("Dictation style", true);
    let presets = Submenu::new("Preset", false);
    let styles: Vec<_> = Style::ALL
        .into_iter()
        .map(|style| {
            (
                style,
                CheckMenuItem::new(style.label(), true, style == Style::Prose, None),
            )
        })
        .collect();
    for (_, item) in &styles {
        style_menu.append(item)?;
    }
    let show_screen = MenuItem::new("Show screen", false, None);
    let show_area = MenuItem::new("Show area…", false, None);
    let copy_last_show = MenuItem::new("Copy last Show", false, None);
    let open_last_show = MenuItem::new("Open last Show", false, None);
    let shows = MenuItem::new("Shows…", true, None);
    let settings = MenuItem::new("Settings…", true, None);
    let quit = MenuItem::new("Quit Speakeasy", true, None);
    menu.append_items(&[
        &status,
        &PredefinedMenuItem::separator(),
        &pause,
        &toggle,
        &cancel,
        &paste_last,
        &style_menu,
        &presets,
        &PredefinedMenuItem::separator(),
        &show_screen,
        &show_area,
        &copy_last_show,
        &open_last_show,
        &shows,
        &PredefinedMenuItem::separator(),
        &settings,
        &quit,
    ])?;
    let icon = build_icon(menu, cx.global::<Services>().config.theme)?;
    let (commands, refreshes) = queue_commands(
        [
            (settings.id().clone(), TrayCommand::Settings),
            (shows.id().clone(), TrayCommand::Shows),
            (pause.id().clone(), TrayCommand::Pause),
            (toggle.id().clone(), TrayCommand::Toggle),
            (cancel.id().clone(), TrayCommand::Cancel),
            (paste_last.id().clone(), TrayCommand::PasteLast),
            (show_screen.id().clone(), TrayCommand::Show(Scope::Screen)),
            (show_area.id().clone(), TrayCommand::Show(Scope::Area)),
            (
                copy_last_show.id().clone(),
                TrayCommand::LastShow(LastShow::Copy),
            ),
            (
                open_last_show.id().clone(),
                TrayCommand::LastShow(LastShow::Open),
            ),
            (quit.id().clone(), TrayCommand::Quit),
        ]
        .into_iter()
        .chain(
            styles
                .iter()
                .map(|(style, item)| (item.id().clone(), TrayCommand::Style(*style))),
        ),
    );
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
        paste_last,
        styles,
        presets,
        preset_items: Vec::new(),
        show_screen,
        show_area,
        copy_last_show,
        open_last_show,
        drawn: None,
    };
    let mut changes = TrayChanges::new(cx);
    let updates = cx.spawn(async move |cx| {
        loop {
            let refresh = tokio::select! {
                state = changes.next(cx) => {
                    let Some(state) = state else { break };
                    presenter.present(state);
                    false
                },
                requested = refreshes.recv() => {
                    if requested.is_err() { break; }
                    true
                },
            };
            if refresh {
                let Some(state) = changes.refresh(cx) else {
                    break;
                };
                presenter.present(state);
            }
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

fn queue_commands(
    items: impl IntoIterator<Item = (MenuId, TrayCommand)>,
) -> (
    async_channel::Receiver<TrayCommand>,
    async_channel::Receiver<()>,
) {
    let items: Vec<_> = items.into_iter().collect();
    let (queue, commands) = async_channel::bounded(COMMAND_BACKLOG);
    let (refresh, refreshes) = async_channel::bounded(1);
    let menu_queue = queue.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some((_, command)) = items.iter().find(|(id, _)| *id == event.id) {
            offer_menu(&menu_queue, &refresh, command.clone());
        } else if let Some(name) = event.id.as_ref().strip_prefix(PRESET_ID) {
            offer_menu(&menu_queue, &refresh, TrayCommand::Preset(name.to_owned()));
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
    (commands, refreshes)
}

fn offer_menu(
    queue: &async_channel::Sender<TrayCommand>,
    refresh: &async_channel::Sender<()>,
    command: TrayCommand,
) {
    let checked = matches!(&command, TrayCommand::Style(_) | TrayCommand::Preset(_));
    offer(queue, command);
    if checked {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A full refresh lane already promises to repair every native check; a closed lane means the tray was disposed"
        )]
        let _ = refresh.try_send(());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_and_rejected_check_clicks_still_request_authoritative_presentation() {
        let (queue, commands) = async_channel::bounded(1);
        let (refresh, refreshes) = async_channel::bounded(1);
        offer_menu(&queue, &refresh, TrayCommand::Style(Style::Prose));
        assert!(refreshes.try_recv().is_ok());
        offer_menu(
            &queue,
            &refresh,
            TrayCommand::Preset("Public fixture".into()),
        );
        assert!(
            refreshes.try_recv().is_ok(),
            "A dropped command left its native check flipped"
        );
        assert!(matches!(
            commands.try_recv(),
            Ok(TrayCommand::Style(Style::Prose))
        ));
        assert!(
            commands.try_recv().is_err(),
            "A full queue admitted the preset click"
        );
        for _ in 0..3 {
            offer_menu(&queue, &refresh, TrayCommand::Style(Style::Prose));
        }
        assert_eq!(commands.len(), 1);
        assert_eq!(
            refreshes.len(),
            1,
            "Redraws must coalesce without losing the last repair"
        );
        assert!(refreshes.try_recv().is_ok());
        assert!(matches!(
            commands.try_recv(),
            Ok(TrayCommand::Style(Style::Prose))
        ));
    }
}
