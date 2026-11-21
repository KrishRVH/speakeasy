use std::{
    fs,
    path::{Path, PathBuf},
};

use gpui::{App, Context, Task, Window, point, px};
use speakeasy_platform::OwnedThread;

use super::{Action as SettingsAction, Notice, Settings, is_demo};
use crate::{gpui_ext::EntityUpdate as _, shell::Services};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Prepare,
    Permissions,
    Practice,
    Ready,
}

impl Step {
    pub(super) const fn heading(self) -> &'static str {
        match self {
            Self::Prepare => "Prepare local speech",
            Self::Permissions if cfg!(target_os = "macos") => "Enable dictation",
            Self::Permissions => "Before you try it",
            Self::Practice => "Try your shortcut",
            Self::Ready => "You're ready",
        }
    }

    pub(super) const fn position(self) -> &'static str {
        match self {
            Self::Prepare => "1 of 4",
            Self::Permissions => "2 of 4",
            Self::Practice => "3 of 4",
            Self::Ready => "4 of 4",
        }
    }

    const fn next(self, readiness: Readiness) -> Self {
        match self {
            Self::Prepare if readiness.prepared => Self::Permissions,
            Self::Permissions if readiness.running => Self::Practice,
            Self::Practice if readiness.recognized => Self::Ready,
            _ => self,
        }
    }

    const fn back(self) -> Self {
        match self {
            Self::Prepare | Self::Permissions => Self::Prepare,
            Self::Practice => Self::Permissions,
            Self::Ready => Self::Practice,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Readiness {
    prepared: bool,
    running: bool,
    recognized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Replay,
    Next,
    Back,
    Finish,
}

pub(super) struct Completion {
    _thread: OwnedThread,
    _follow: Task<()>,
}

pub(crate) fn pending(settings: &Path) -> bool {
    !marker(settings).is_file()
}

fn complete(settings: &Path) -> std::io::Result<()> {
    if let Some(parent) = settings.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(marker(settings), [])
}

fn marker(settings: &Path) -> PathBuf {
    settings.with_added_extension("onboarded")
}

pub(super) fn speech_failed(cx: &App) -> bool {
    !is_demo(cx) && !cx.global::<Services>().speech_checked()
}

impl Settings {
    pub(super) fn onboarding_next(&self, step: Step, cx: &Context<'_, Self>) -> Step {
        let demo = is_demo(cx);
        step.next(Readiness {
            prepared: demo
                || (self.config.speech_configured()
                    && !speech_failed(cx)
                    && !self.unsaved(cx)
                    && self.saving.is_none()),
            running: demo || cx.global::<Services>().is_running(),
            recognized: self.practice.recognized(),
        })
    }

    pub(super) fn onboarding_primary(
        &self,
        step: Step,
        cx: &Context<'_, Self>,
    ) -> (&'static str, Option<SettingsAction>) {
        let demo = is_demo(cx);
        let running = demo || cx.global::<Services>().is_running();
        let advance = || {
            (self.onboarding_next(step, cx) != step)
                .then_some(SettingsAction::Onboarding(Action::Next))
        };
        match step {
            Step::Prepare if self.resetting() => ("Starting over…", None),
            Step::Prepare if self.unreadable => ("Continue", None),
            Step::Prepare if self.engine_work.installing().is_some() => ("Setting up…", None),
            Step::Prepare if self.engine_work.is_pausing() => ("Pausing download…", None),
            Step::Prepare | Step::Practice if self.saving.is_some() => ("Saving…", None),
            Step::Prepare if !demo && (!self.config.speech_configured() || speech_failed(cx)) => (
                "Set up automatically · 0.8 GB",
                (!self.engine_work.is_busy()).then_some(SettingsAction::Setup),
            ),
            Step::Permissions | Step::Practice if !running => {
                ("Retry dictation", Some(SettingsAction::Resume))
            },
            Step::Practice if !self.practice.recognized() && self.practice.live.is_none() => (
                if demo {
                    "Simulate dictation"
                } else {
                    "Start practice"
                },
                Some(SettingsAction::Practice),
            ),
            Step::Ready => (
                "Start using Speakeasy",
                Some(SettingsAction::Onboarding(Action::Finish)),
            ),
            Step::Permissions => ("Try dictation", advance()),
            Step::Prepare | Step::Practice => ("Continue", advance()),
        }
    }

    pub(super) fn onboarding_action(
        &mut self,
        action: Action,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.onboarding_completion.is_some() {
            return;
        }
        if action == Action::Finish {
            self.finish_onboarding(cx);
            return;
        }
        let next = match (self.onboarding, action) {
            (_, Action::Replay) => Step::Prepare,
            (Some(step), Action::Next) => {
                let next = self.onboarding_next(step, cx);
                if next == step {
                    return;
                }
                next
            },
            (Some(step), Action::Back) => step.back(),
            _ => return,
        };
        self.practice.stop();
        self.notice = None;
        self.onboarding = Some(next);
        self.navigation.scroll.set_offset(point(px(0.0), px(0.0)));
        self.onboarding_focus.focus(window, cx);
        if next == Step::Practice && !is_demo(cx) {
            self.toggle_practice(cx);
        }
    }

    fn finish_onboarding(&mut self, cx: &Context<'_, Self>) {
        self.practice.stop();
        if is_demo(cx) {
            self.onboarding = None;
            return;
        }
        let settings = cx.global::<Services>().path.clone();
        let (sender, receiver) = async_channel::bounded(1);
        let Ok(thread) = OwnedThread::spawn("onboarding-save", move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The completion marker still writes when its settings window has closed"
            )]
            let _ = sender.try_send(complete(&settings));
        }) else {
            self.notice = Some(Notice::Problem(
                "Cannot remember setup completion. Try again.".into(),
            ));
            return;
        };
        self.notice = Some(Notice::Note("Remembering setup completion…".into()));
        let follow = cx.spawn(async move |this, cx| {
            let completed = matches!(receiver.recv().await, Ok(Ok(())));
            this.update_if_alive(cx, |view, cx| {
                view.onboarding_completion = None;
                if completed {
                    view.onboarding = None;
                    view.notice = None;
                } else {
                    view.notice = Some(Notice::Problem(
                        "Cannot remember setup completion. Check that the settings folder is writable, then try again.".into(),
                    ));
                }
                cx.notify();
            });
        });
        self.onboarding_completion = Some(Completion {
            _thread: thread,
            _follow: follow,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREPARED: Readiness = Readiness {
        prepared: true,
        running: false,
        recognized: false,
    };

    const RUNNING: Readiness = Readiness {
        running: true,
        ..PREPARED
    };

    #[test]
    fn permissions_remain_reachable_when_the_shortcut_cannot_start() {
        assert_eq!(Step::Prepare.next(Readiness::default()), Step::Prepare);
        assert_eq!(Step::Prepare.next(PREPARED), Step::Permissions);
        assert_eq!(Step::Permissions.next(PREPARED), Step::Permissions);
        assert_eq!(Step::Permissions.next(RUNNING), Step::Practice);
    }

    #[test]
    fn only_a_recognized_result_unlocks_success() {
        assert_eq!(Step::Practice.next(RUNNING), Step::Practice);
        let recognized = Readiness {
            recognized: true,
            ..RUNNING
        };
        assert_eq!(Step::Practice.next(recognized), Step::Ready);
        assert_eq!(Step::Ready.back(), Step::Practice);
    }

    #[test]
    fn completion_survives_restart_without_changing_settings() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let settings = directory.path().join("nested/settings.json");
        assert!(pending(&settings));
        complete(&settings)?;
        assert!(!pending(&settings));
        assert!(!settings.exists());
        fs::write(&settings, b"user settings")?;
        complete(&settings)?;
        assert_eq!(fs::read(&settings)?, b"user settings");
        assert!(pending(&settings.with_file_name("other.json")));
        Ok(())
    }

    #[test]
    fn failed_completion_remains_pending() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let parent = directory.path().join("file");
        fs::write(&parent, [])?;
        let settings = parent.join("settings.json");
        assert!(complete(&settings).is_err());
        assert!(pending(&settings));
        Ok(())
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
pub(super) mod native_tests;
