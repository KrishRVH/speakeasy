use std::path::PathBuf;

use gpui::Context;

use super::{Export, Review, Write};
use crate::{
    gpui_ext::EntityUpdate as _,
    show::edits::{Edit, Written},
};

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Archive,
    Video,
}

impl Review {
    pub(super) const fn can_export(&self) -> bool {
        self.export.is_none() && !matches!(self.write, Some(Write::Deleting { .. }))
    }

    pub(super) fn export(&mut self, kind: Kind, cx: &mut Context<'_, Self>) {
        if !self.can_export() || self.detail.is_none() {
            return;
        }
        let Some(key) = &self.library.selected else {
            return;
        };
        let directory = key.directory.clone();
        let name = directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("speakeasy-show");
        let extension = match kind {
            Kind::Archive => "zip",
            Kind::Video => "mp4",
        };
        let prompt = cx.prompt_for_new_path(
            directory.parent().unwrap_or(&directory),
            Some(&format!("{name}.{extension}")),
        );
        let task = cx.spawn(async move |view, cx| {
            let result = prompt
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
            view.update_if_alive(cx, |view, cx| {
                view.export = None;
                match result {
                    Ok(Some(destination)) => view.export_to(directory, destination, kind, cx),
                    Ok(None) => {},
                    Err(_) => view.notice = Some(
                        "Cannot open the export dialog. Try again, or use speakeasy show export."
                            .into(),
                    ),
                }
                cx.notify();
            });
        });
        self.export = Some(Export::Choosing { _task: task });
        cx.notify();
    }

    pub(super) fn export_to(
        &mut self,
        directory: PathBuf,
        destination: PathBuf,
        kind: Kind,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.library.is_selected(&directory) || !self.can_export() {
            return;
        }
        let (edit, cancel) = match kind {
            Kind::Video => {
                let (cancel, receiver) = async_channel::bounded(1);
                let edit = Edit::Video {
                    directory: directory.clone(),
                    destination,
                    cancel: receiver,
                };
                (edit, Some(cancel))
            },
            Kind::Archive => {
                let edit = Edit::Export {
                    directory: directory.clone(),
                    destination,
                };
                (edit, None)
            },
        };
        let work = match (self.actions.edit)(edit, self.fixture.clone(), cx) {
            Ok(work) => work,
            Err(error) => {
                self.notice = Some(format!("Cannot export this Show: {error:#}"));
                return;
            },
        };
        self.notice = Some(
            match kind {
                Kind::Video => {
                    "Exporting video with annotations, available voice and timed captions…"
                },
                Kind::Archive => "Exporting saved images and narration…",
            }
            .into(),
        );
        let task = cx.spawn(async move |view, cx| {
            let result = work
                .recv()
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
            view.update_if_alive(cx, |view, cx| {
                view.export = None;
                if view.library.is_selected(&directory) {
                    view.notice = Some(match result {
                        Ok(Written::Exported(exported)) => format!(
                            "Exported to {}. This copy does not expire automatically.",
                            exported.path.display()
                        ),
                        Ok(Written::Video(exported)) => exported.note(),
                        Ok(_) => "The export returned no archive. Try again.".into(),
                        Err(error) => format!("Cannot export this Show: {error:#}"),
                    });
                }
                cx.notify();
            });
        });
        self.export = Some(Export::Running {
            _task: task,
            cancel,
        });
        cx.notify();
    }
}
