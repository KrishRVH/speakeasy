use std::path::Path;

use gpui::Context;

use super::{Confirm, Edit, Kind, Review, Write, library::Row, retention};
use crate::gpui_ext::EntityUpdate as _;

impl Review {
    pub(super) fn can_delete(&self) -> bool {
        self.root.is_some()
            && self.listing.is_none()
            && self.opening.is_none()
            && self.loading.is_none()
            && self.write.is_none()
            && self.export.is_none()
            && self.copying.is_none()
    }

    fn deletable(&self, offer: usize) -> Option<&Row> {
        self.library.rows.get(offer).filter(|row| {
            self.can_delete()
                && matches!(row.kind, Kind::Saved)
                && self.library.is_selected(&row.directory)
        })
    }

    pub(super) fn ask_delete(&mut self, offer: usize) {
        if self.deletable(offer).is_some() {
            self.confirm = Some(Confirm::Delete(offer));
        }
    }

    pub(super) fn delete(&mut self, offer: usize, cx: &mut Context<'_, Self>) {
        if self.confirm != Some(Confirm::Delete(offer)) {
            return;
        }
        let Some((parent, row)) = self.parent().zip(self.deletable(offer)) else {
            return;
        };
        let directory = row.directory.clone();
        let target = directory.clone();
        let (edit, receipt) = Edit::new(move || retention::delete(&parent, &target));
        if let Err(error) = (self.actions.edit)(edit, self.fixture(), cx) {
            self.notice = Some(format!("Can't delete this Show: {error:#}"));
            return;
        }
        self.clear_selection(cx);
        self.notice = Some("Deleting Show…".into());
        self.write = Some(Write::Deleting {
            directory: directory.clone(),
            _task: cx.spawn(async move |view, cx| {
                let result = receipt.recv().await.map_err(anyhow::Error::from).flatten();
                view.update_if_alive(cx, |view, cx| view.delete_finished(&directory, result, cx));
            }),
        });
    }

    fn delete_finished(
        &mut self,
        directory: &Path,
        result: anyhow::Result<()>,
        cx: &mut Context<'_, Self>,
    ) {
        self.write = None;
        match result {
            Ok(()) => {
                if self.library.remove(directory) {
                    self.clear_selection(cx);
                    self.notice = Some("Show deleted.".into());
                }
            },
            Err(error) if self.library.is_selected(directory) => {
                if let Some(key) = self.library.selected().cloned() {
                    self.open_selected(key, None, cx);
                }
                self.notice = Some(format!("Can't delete this Show: {error:#}"));
            },
            Err(_) => cx.notify(),
        }
        cx.notify();
    }
}
