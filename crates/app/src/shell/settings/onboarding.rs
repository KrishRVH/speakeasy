use std::{fs, path::Path};

use gpui::{Context, Task, Window, point, px};
use speakeasy_platform::OwnedThread;

use super::{Notice, Settings, is_demo};
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
            Self::Permissions => "Enable dictation",
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

    pub(super) fn next(self, prepared: bool, running: bool, recognized: bool) -> Self {
        match self {
            Self::Prepare if prepared => Self::Permissions,
            Self::Permissions if running => Self::Practice,
            Self::Practice if recognized => Self::Ready,
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
    !settings.with_added_extension("onboarded").is_file()
}

fn complete(settings: &Path) -> std::io::Result<()> {
    if let Some(parent) = settings.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(settings.with_added_extension("onboarded"), [])
}

impl Settings {
    pub(super) fn onboarding_next(&self, step: Step, cx: &Context<'_, Self>) -> Step {
        step.next(
            is_demo(cx)
                || (!self.config.needs_engine() && !self.unsaved(cx) && self.saving.is_none()),
            is_demo(cx) || cx.global::<Services>().is_running(),
            !self.practice.text.trim().is_empty(),
        )
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
            (_, Action::Replay) => {
                if self.saves.needs_save(&self.config)
                    || self.vocabulary.pending(cx)
                    || self.recognition.pending(cx)
                    || self.presets.pending(cx)
                {
                    self.notice = Some(Notice::Note(
                        "Save or revert your changes before replaying setup.".into(),
                    ));
                    return;
                }
                self.close_language(window);
                Step::Prepare
            },
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
        self.onboarding_focus.focus(window);
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
        let thread = OwnedThread::spawn("onboarding-save", move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The completion marker still writes when its settings window has closed"
            )]
            let _ = sender.try_send(complete(&settings));
        });
        let Ok(thread) = thread else {
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

    #[test]
    fn permissions_remain_reachable_when_the_shortcut_cannot_start() {
        assert_eq!(Step::Prepare.next(false, false, false), Step::Prepare);
        assert_eq!(Step::Prepare.next(true, false, false), Step::Permissions);
        assert_eq!(
            Step::Permissions.next(true, false, false),
            Step::Permissions
        );
        assert_eq!(Step::Permissions.next(true, true, false), Step::Practice);
    }

    #[test]
    fn only_a_recognized_result_unlocks_success() {
        assert_eq!(Step::Practice.next(true, true, false), Step::Practice);
        assert_eq!(Step::Practice.next(true, true, true), Step::Ready);
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
