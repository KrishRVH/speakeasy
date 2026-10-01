//! The Linux tray, a `StatusNotifierItem` served by the platform crate. Losing the tray host
//! reveals Settings, which is then the only way back into the app.

use gpui::{App, Global, Task};
use speakeasy_platform::{NativeTray, TrayAction, TrayPresentation};

use super::{
    TrayChanges, TrayCommand, TrayState,
    icon::{IconStyle, raster},
};
use crate::shell;

pub(crate) struct Tray {
    /// Whether a tray host shows the icon; without one, closing Settings quits.
    pub available: bool,
    _events: Task<()>,
    _updates: Task<()>,
    _native: NativeTray,
}

impl Global for Tray {}

pub(crate) fn install(cx: &mut App) -> anyhow::Result<()> {
    let (native, actions) = NativeTray::start()?;
    let presentations = native.updater();
    let events = cx.spawn(async move |cx| {
        while let Ok(action) = actions.recv().await {
            if cx.update(|cx| handle(action, cx)).is_err() {
                break;
            }
        }
    });
    let mut changes = TrayChanges::new(cx);
    let updates = cx.spawn(async move |cx| {
        while let Some(state) = changes.next(cx).await {
            if presentations.send(presentation(state)).await.is_err() {
                break;
            }
        }
    });
    cx.set_global(Tray {
        available: false,
        _events: events,
        _updates: updates,
        _native: native,
    });
    Ok(())
}

fn handle(action: TrayAction, cx: &mut App) {
    match action {
        TrayAction::Available(available) => {
            if cx.has_global::<Tray>() {
                cx.global_mut::<Tray>().available = available;
            }
            if !available {
                shell::reveal(cx);
            }
        },
        TrayAction::Settings => TrayCommand::Settings.run(cx),
        TrayAction::Pause => TrayCommand::Pause.run(cx),
        TrayAction::Toggle => TrayCommand::Toggle.run(cx),
        TrayAction::Cancel => TrayCommand::Cancel.run(cx),
        TrayAction::Quit => TrayCommand::Quit.run(cx),
    }
}

fn presentation(state: &TrayState) -> TrayPresentation {
    TrayPresentation {
        rgba: raster(state.status.indicator, state.theme, IconStyle::Color),
        description: state.status.description.to_string(),
        service: state.service,
        activity: state.activity,
        interactive: state.interactive,
        desktop_ready: state.desktop_ready,
    }
}
