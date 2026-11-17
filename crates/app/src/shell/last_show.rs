use std::path::Path;

use gpui::{App, BorrowAppContext as _, Task};

use super::Services;
use crate::{
    runtime::Report,
    show::{
        SavedShow,
        copy::{Outcome, Request},
    },
};

#[derive(Clone, Copy)]
pub(crate) enum LastShow {
    Copy,
    Open,
}

impl LastShow {
    pub(crate) fn run(self, cx: &mut App) {
        cx.update_global::<Services, _>(|services, cx| {
            let Some(show) = services.recallable_show(&services.output.borrow()).cloned() else {
                return;
            };
            let task = match self {
                Self::Copy => copy(services, show, cx),
                Self::Open => Ok(open(show, cx)),
            };
            match task {
                Ok(task) => services.last_show = Some(task),
                Err(error) => services.report(&error),
            }
        });
    }
}

fn copy(services: &mut Services, show: SavedShow, cx: &App) -> anyhow::Result<Task<()>> {
    let receipt = services.copy_show(Request::Saved(show.clone()))?;
    Ok(cx.spawn(async move |cx| {
        let report = match receipt.await {
            Ok(Outcome::Copied) => Report::ShowCopied,
            Ok(Outcome::Missing) => Report::ShowGone(show),
            Err(error) => Report::Failed(error),
        };
        cx.update(|cx| cx.global::<Services>().report(report));
    }))
}

fn open(show: SavedShow, cx: &App) -> Task<()> {
    let found = cx.background_executor().spawn({
        let show = show.clone();
        async move { show.folder().map(|folder| folder.map(Path::to_owned)) }
    });
    cx.spawn(async move |cx| {
        let found = found.await;
        cx.update(|cx| match found {
            Ok(Some(folder)) => cx.open_with_system(&folder),
            Ok(None) => cx.global::<Services>().report(Report::ShowGone(show)),
            Err(error) => cx.global::<Services>().report(&error),
        });
    })
}
