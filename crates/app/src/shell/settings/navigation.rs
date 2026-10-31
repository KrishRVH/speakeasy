//! Settings pages share one unsaved draft. Leaving dictation revokes private practice and closes
//! its language popup; navigation keeps keyboard focus outside the controls it removes.

use gpui::{Context, FocusHandle, ScrollHandle, Window, point, px};

use super::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    Dictation,
    Words,
    Advanced,
}

impl Page {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Dictation => "Dictation",
            Self::Words => "Words",
            Self::Advanced => "Advanced",
        }
    }
}

pub(super) struct Navigation {
    pub(super) page: Page,
    pub(super) scroll: ScrollHandle,
    dictation: FocusHandle,
    words: FocusHandle,
    advanced: FocusHandle,
}

impl Navigation {
    pub(super) fn new(cx: &Context<'_, Settings>) -> Self {
        Self {
            page: Page::Dictation,
            scroll: ScrollHandle::new(),
            dictation: cx.focus_handle().tab_stop(true),
            words: cx.focus_handle().tab_stop(true),
            advanced: cx.focus_handle().tab_stop(true),
        }
    }

    pub(super) const fn focus(&self, page: Page) -> &FocusHandle {
        match page {
            Page::Dictation => &self.dictation,
            Page::Words => &self.words,
            Page::Advanced => &self.advanced,
        }
    }
}

impl Settings {
    pub(super) fn open_page(&mut self, page: Page, window: &mut Window) {
        if self.navigation.page != page {
            self.practice.stop();
            self.close_language(window);
            self.navigation.page = page;
            self.navigation.scroll.set_offset(point(px(0.0), px(0.0)));
        }
        self.navigation.focus(page).focus(window);
    }
}
