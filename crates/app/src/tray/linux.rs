use super::{TrayState, raster, state};
use crate::shell::{self, Services};
use gpui::{App, Global, Task};
use speakeasy_platform::{Input, NativeTray, TrayAction, TrayPresentation};

pub struct Tray {
    pub available: bool,
    _events: Task<()>,
    _updates: Task<()>,
    _native: NativeTray,
}
impl Global for Tray {}
pub fn install(cx: &mut App) -> anyhow::Result<()> {
    let (native, events) = NativeTray::start()?;
    let updater = native.updater();
    let event_task = cx.spawn(async move |cx| {
        while let Ok(event) = events.recv().await {
            if cx
                .update(|cx| match event {
                    TrayAction::Available(available) => {
                        if cx.has_global::<Tray>() {
                            cx.global_mut::<Tray>().available = available;
                        }
                        if !available {
                            shell::reveal(cx);
                        }
                    }
                    TrayAction::Settings => shell::reveal(cx),
                    TrayAction::Pause => shell::toggle_enabled(cx),
                    TrayAction::Toggle => shell::send(Input::Toggle, cx),
                    TrayAction::Cancel => shell::send(Input::Cancel, cx),
                    TrayAction::Quit => cx.quit(),
                })
                .is_err()
            {
                break;
            }
        }
    });
    let mut updates = cx.global::<Services>().output.subscribe();
    let update_task = cx.spawn(async move |cx| {
        let mut previous: Option<TrayState> = None;
        loop {
            let snapshot = updates.borrow_and_update().clone();
            let presentation = cx.update(|cx| {
                let services = cx.global::<Services>();
                let next = state(services, &snapshot);
                if previous.as_ref() == Some(&next) {
                    return None;
                }
                let presentation = TrayPresentation {
                    rgba: raster(next.indicator, next.theme, false),
                    description: next.description.clone(),
                    running: next.running,
                    pausing: next.pausing,
                    capturing: next.capturing,
                    active: next.active,
                    enabled: !services.demo,
                    ready: next.ready,
                };
                previous = Some(next);
                Some(presentation)
            });
            match presentation {
                Ok(Some(presentation)) => {
                    if updater.send(presentation).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => break,
            }
            if updates.changed().await.is_err() {
                break;
            }
        }
    });
    cx.set_global(Tray {
        available: false,
        _events: event_task,
        _updates: update_task,
        _native: native,
    });
    Ok(())
}
