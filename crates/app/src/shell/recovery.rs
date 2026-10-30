//! Copy last Show and Open last Show in Speakeasy's menu. Each acts on exactly the Show the latest
//! snapshot names and never looks for another once that one has gone.

use std::path::Path;

use gpui::{App, BorrowAppContext as _};

use super::Services;
use crate::{gpui_ext::AppUpdate as _, runtime::Report, show::SavedShow};

/// What the menu does with the last Show.
#[derive(Clone, Copy)]
pub(crate) enum LastShow {
    /// Copies its summary as its folder holds it now, and says so; it never pastes.
    Copy,
    /// Opens its folder in the file manager.
    Open,
}

impl LastShow {
    pub(crate) fn run(self, cx: &mut App) {
        match self {
            Self::Copy => recover(
                |show| speakeasy_platform::copy_text(&show.summary()?),
                |(), cx| cx.global::<Services>().report(Report::ShowCopied),
                cx,
            ),
            Self::Open => recover(
                |show| show.folder().map(Path::to_owned),
                |folder, cx| cx.open_with_system(&folder),
                cx,
            ),
        }
    }
}

/// Runs `work` on the last Show off the UI thread, replacing any recovery still under way, then
/// hands its result to `done`, or reports its failure as Services reports any other.
fn recover<T: Send + 'static>(
    work: impl FnOnce(SavedShow) -> anyhow::Result<T> + Send + 'static,
    done: impl FnOnce(T, &mut App) + 'static,
    cx: &mut App,
) {
    cx.update_global::<Services, _>(|services, cx| {
        let Some(show) = services.output.borrow().last_show.clone() else {
            return;
        };
        let work = cx.background_executor().spawn(async move { work(show) });
        services.recovery = Some(cx.spawn(async move |cx| {
            let result = work.await;
            cx.update_if_running(|cx| match result {
                Ok(value) => done(value, cx),
                Err(error) => cx.global::<Services>().report(&error),
            });
        }));
    });
}
