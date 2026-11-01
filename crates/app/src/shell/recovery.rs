use std::path::Path;

use anyhow::anyhow;
use gpui::{App, BorrowAppContext as _};

use super::Services;
use crate::{gpui_ext::AppUpdate as _, runtime::Report};

#[derive(Clone, Copy)]
pub(crate) enum LastShow {
    Copy,
    Open,
}

impl LastShow {
    pub(crate) fn run(self, cx: &mut App) {
        match self {
            Self::Copy => copy(cx),
            Self::Open => open(cx),
        }
    }
}

fn copy(cx: &mut App) {
    let services = cx.global_mut::<Services>();
    let Some(show) = services.recallable_show(&services.output.borrow()).cloned() else {
        return;
    };
    let receipt = match services.copy_show(crate::show::copy::Request::Saved(show.clone())) {
        Ok(receipt) => receipt,
        Err(error) => {
            cx.global::<Services>().report(&error);
            return;
        },
    };
    let copying = cx.spawn(async move |cx| {
        let result = receipt.await;
        cx.update_if_running(|cx| {
            let services = cx.global::<Services>();
            match result {
                Ok(crate::show::copy::Outcome::Copied) => services.report(Report::ShowCopied),
                Ok(crate::show::copy::Outcome::Missing) => services.report(Report::ShowGone(show)),
                Err(error) => services.report(&anyhow!("{error}")),
            }
        });
    });
    cx.global_mut::<Services>().recovery = Some(copying);
}

fn open(cx: &mut App) {
    cx.update_global::<Services, _>(|services, cx| {
        let Some(show) = services.recallable_show(&services.output.borrow()).cloned() else {
            return;
        };
        let found = cx.background_executor().spawn({
            let show = show.clone();
            async move { show.folder().map(|folder| folder.map(Path::to_owned)) }
        });
        services.recovery = Some(cx.spawn(async move |cx| {
            let found = found.await;
            cx.update_if_running(|cx| match found {
                Ok(Some(folder)) => cx.open_with_system(&folder),
                Ok(None) => cx.global::<Services>().report(Report::ShowGone(show)),
                Err(error) => cx.global::<Services>().report(&error),
            });
        }));
    });
}
