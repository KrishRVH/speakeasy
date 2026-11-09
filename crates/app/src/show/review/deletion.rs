use gpui::Context;

use super::{Confirm, Kind, Review, Write, library::Offered};
use crate::{
    gpui_ext::EntityUpdate as _,
    show::edits::{Edit, Written},
};

impl Review {
    pub(super) fn can_delete(&self) -> bool {
        self.directory.is_some()
            && self.listing.is_none()
            && self.opening.is_none()
            && self.loading.is_none()
            && self.write.is_none()
            && self.export.is_none()
            && self.copying.is_none()
    }

    pub(super) fn ask_delete(&mut self, offer: Offered) {
        if self.can_delete()
            && let Some(row) = self.library.offered(offer)
            && matches!(row.kind, Kind::Saved)
            && self.library.is_selected(&row.directory)
        {
            self.confirm = Some(Confirm::Delete(offer));
        }
    }

    pub(super) fn delete(&mut self, offer: Offered, cx: &mut Context<'_, Self>) {
        if !self.can_delete() || self.confirm != Some(Confirm::Delete(offer)) {
            return;
        }
        let Some(row) = self.library.offered(offer).filter(|row| {
            matches!(row.kind, Kind::Saved) && self.library.is_selected(&row.directory)
        }) else {
            return;
        };
        let Some(parent) = self.directory.clone() else {
            return;
        };
        let directory = row.directory.clone();
        let receipt = match (self.actions.edit)(
            Edit::Delete {
                parent,
                directory: directory.clone(),
            },
            self.fixture.clone(),
            cx,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                self.notice = Some(format!("Cannot delete this Show: {error:#}"));
                return;
            },
        };
        self.clear_selection(cx);
        self.notice = Some("Deleting Show…".into());
        self.write = Some(Write::Deleting {
            directory: directory.clone(),
            _task: cx.spawn(async move |view, cx| {
                let result = receipt
                    .recv()
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|result| result);
                view.update_if_alive(cx, |view, cx| view.delete_finished(&directory, result, cx));
            }),
        });
    }

    fn delete_finished(
        &mut self,
        directory: &std::path::Path,
        result: anyhow::Result<Written>,
        cx: &mut Context<'_, Self>,
    ) {
        self.write = None;
        let result = result.and_then(|written| match written {
            Written::Deleted(deleted) => Ok(deleted),
            _ => anyhow::bail!("Show deletion returned an unexpected result. Refresh Shows."),
        });
        match result {
            Ok(deleted) => {
                let selected = self.library.remove(directory);
                let checkpoint_selected = deleted
                    .checkpoint
                    .as_ref()
                    .is_some_and(|path| self.library.remove(path));
                if selected || checkpoint_selected {
                    self.clear_selection(cx);
                    self.notice = Some(deleted.cleanup.map_or_else(
                        || "Show deleted.".into(),
                        |error| format!("Show removed from the library, but some files still need cleanup: {error}. Close open Show files; Speakeasy retries cleanup within the hour."),
                    ));
                }
            },
            Err(error) if self.library.is_selected(directory) => {
                if let Some(key) = self.library.selected.clone() {
                    self.open_selected(key, None, cx);
                }
                self.notice = Some(format!("Cannot delete this Show: {error:#}"));
            },
            Err(_) => cx.notify(),
        }
        cx.notify();
    }
}
