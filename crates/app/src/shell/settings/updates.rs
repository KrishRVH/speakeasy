use gpui::{Context, Task};

use super::{Notice, Settings, is_demo, update_notice};
use crate::{
    gpui_ext::EntityUpdate as _,
    shell::Services,
    updates::{Check, Version},
};

#[derive(Default)]
pub(super) enum Update {
    #[default]
    Idle,
    Checking {
        operation: Check,
        _follow: Task<()>,
    },
    Available(Version),
    #[cfg(target_os = "windows")]
    Downloading {
        operation: crate::updates::Download,
        _follow: Task<()>,
        version: Version,
        cancelled: bool,
    },
    #[cfg(target_os = "windows")]
    Ready(crate::updates::Installer),
}

impl Update {
    pub(super) const fn version(&self) -> Option<Version> {
        match self {
            Self::Available(version) => Some(*version),
            #[cfg(target_os = "windows")]
            Self::Downloading { version, .. } => Some(*version),
            #[cfg(target_os = "windows")]
            Self::Ready(installer) => Some(installer.version()),
            Self::Idle | Self::Checking { .. } => None,
        }
    }

    pub(super) const fn busy(&self) -> bool {
        match self {
            Self::Checking { .. } => true,
            #[cfg(target_os = "windows")]
            Self::Downloading { .. } => true,
            _ => false,
        }
    }

    pub(super) fn cancel(&mut self) {
        match self {
            Self::Checking { operation, .. } => operation.request_stop(),
            #[cfg(target_os = "windows")]
            Self::Downloading {
                operation,
                cancelled,
                ..
            } => {
                *cancelled = true;
                operation.request_stop();
            },
            _ => {},
        }
    }

    pub(super) fn drained(&mut self) -> bool {
        let finished = match self {
            Self::Checking { operation, .. } => operation.is_finished(),
            #[cfg(target_os = "windows")]
            Self::Downloading { operation, .. } => operation.is_finished(),
            _ => true,
        };
        if finished {
            *self = Self::Idle;
        }
        finished
    }
}

impl Settings {
    pub(super) fn check_updates(&mut self, cx: &Context<'_, Self>) {
        if is_demo(cx) || self.update.busy() || cx.global::<Services>().is_quitting() {
            return;
        }
        let Ok(operation) = Check::start() else {
            self.notice = Some(Notice::Problem(
                "Cannot start the update check. Try again.".into(),
            ));
            return;
        };
        let outcome = operation.outcome();
        self.notice = Some(Notice::Note("Checking the latest GitHub release…".into()));
        let follow = cx.spawn(async move |this, cx| {
            let result = outcome.await;
            this.update_if_alive(cx, |view, cx| {
                view.update = Update::Idle;
                if cx.global::<Services>().is_quitting() {
                    return;
                }
                view.notice = Some(match result {
                    Ok(version) => {
                        view.update = version.map_or(Update::Idle, Update::Available);
                        update_notice(version)
                    },
                    Err(error) => Notice::Problem(error.to_string().into()),
                });
                cx.notify();
            });
        });
        self.update = Update::Checking {
            operation,
            _follow: follow,
        };
    }

    #[cfg(target_os = "windows")]
    pub(super) fn download_update(&mut self, cx: &Context<'_, Self>) {
        let Update::Available(version) = self.update else {
            return;
        };
        if is_demo(cx) || cx.global::<Services>().is_quitting() {
            return;
        }
        let (operation, mut progress) = match crate::updates::Download::prepare(
            version,
            cx.global::<Services>().path.clone(),
        ) {
            Ok(download) => download,
            Err(error) => {
                self.notice = Some(Notice::Problem(error.to_string().into()));
                return;
            },
        };
        let outcome = operation.outcome();
        let follow = cx.spawn(async move |this, cx| {
            tokio::pin!(outcome);
            let result = loop {
                tokio::select! {
                    biased;
                    result = &mut outcome => break result,
                    changed = progress.changed() => {
                        if changed.is_err() { break outcome.await; }
                        let progress = *progress.borrow_and_update();
                        this.update_if_alive(cx, |view, cx| {
                            if matches!(view.update, Update::Downloading { cancelled: false, .. }) {
                                let message = progress.done.saturating_mul(100).checked_div(progress.total).map_or_else(
                                    || progress.stage.to_owned(),
                                    |percent| format!("{}: {percent}%", progress.stage),
                                );
                                view.notice = Some(Notice::Note(message.into()));
                                cx.notify();
                            }
                        });
                        gpui::Timer::after(super::PROGRESS_THROTTLE).await;
                    },
                }
            };
            this.update_if_alive(cx, |view, cx| {
                let cancelled = matches!(view.update, Update::Downloading { cancelled: true, .. });
                view.update = Update::Available(version);
                if cx.global::<Services>().is_quitting() { return; }
                if cancelled {
                    view.notice = Some(Notice::Note("Update download cancelled.".into()));
                    cx.notify();
                    return;
                }
                view.notice = Some(match result {
                    Ok(Some(installer)) => {
                        view.update = Update::Ready(installer);
                        Notice::Note("Update verified. Save your changes, then choose Install and restart.".into())
                    },
                    Ok(None) => Notice::Note("Update download cancelled.".into()),
                    Err(error) => Notice::Problem(error.to_string().into()),
                });
                cx.notify();
            });
        });
        self.update = Update::Downloading {
            operation,
            _follow: follow,
            version,
            cancelled: false,
        };
        self.notice = Some(Notice::Note("Authenticating update…".into()));
    }

    #[cfg(target_os = "windows")]
    pub(super) fn install_update(&mut self, cx: &mut Context<'_, Self>) {
        if is_demo(cx) || cx.global::<Services>().is_quitting() {
            return;
        }
        if self.unsaved(cx) || !self.saves.is_idle() {
            self.notice = Some(Notice::Problem(
                "Save your settings before installing the update.".into(),
            ));
            return;
        }
        if cx.global::<Services>().output.borrow().phase.is_active() {
            self.notice = Some(Notice::Note(
                "Finish or cancel the current recording before updating.".into(),
            ));
            return;
        }
        let previous = std::mem::take(&mut self.update);
        if let Update::Ready(installer) = previous {
            crate::shell::shutdown::request_install(installer, cx);
        } else {
            self.update = previous;
        }
    }
}
