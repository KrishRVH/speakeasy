use std::path::PathBuf;

use gpui::Context;

use super::Review;
use crate::{
    gpui_ext::EntityUpdate as _,
    show::edits::{Edit, Written},
};

impl Review {
    pub(super) fn export(&mut self, video: bool, cx: &mut Context<'_, Self>) {
        if self.exporting.is_some() || self.detail.is_none() {
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
        let extension = if video { "mp4" } else { "zip" };
        let prompt = cx.prompt_for_new_path(
            directory.parent().unwrap_or(&directory),
            Some(&format!("{name}.{extension}")),
        );
        self.exporting = Some(cx.spawn(async move |view, cx| {
            let result = prompt
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
            view.update_if_alive(cx, |view, cx| {
                view.exporting = None;
                match result {
                    Ok(Some(destination)) => view.export_to(directory, destination, video, cx),
                    Ok(None) => {},
                    Err(_) => view.notice = Some(
                        "Cannot open the export dialog. Try again, or use speakeasy show export."
                            .into(),
                    ),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn export_to(
        &mut self,
        directory: PathBuf,
        destination: PathBuf,
        video: bool,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.library.is_selected(&directory) || self.exporting.is_some() {
            return;
        }
        let edit = if video {
            let (cancel, receiver) = async_channel::bounded(1);
            self.export_cancel = Some(cancel);
            Edit::Video {
                directory: directory.clone(),
                destination,
                cancel: receiver,
            }
        } else {
            Edit::Export {
                directory: directory.clone(),
                destination,
            }
        };
        let work = match (self.actions.edit)(edit, self.fixture.clone(), cx) {
            Ok(work) => work,
            Err(error) => {
                self.export_cancel = None;
                self.notice = Some(format!("Cannot export this Show: {error:#}"));
                return;
            },
        };
        self.notice = Some(
            if video {
                "Exporting video with annotations, available voice and timed captions…"
            } else {
                "Exporting saved images and narration…"
            }
            .into(),
        );
        self.exporting = Some(cx.spawn(async move |view, cx| {
            let result = work
                .recv()
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
            view.update_if_alive(cx, |view, cx| {
                view.exporting = None;
                view.export_cancel = None;
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
        }));
        cx.notify();
    }
}
