use gpui::{Context, FocusHandle, ScrollHandle, Window, point, px};

use super::Settings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    Dictation,
    Words,
    General,
}

impl Page {
    pub(super) const ALL: [Self; 3] = [Self::Dictation, Self::Words, Self::General];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Dictation => "Dictation",
            Self::Words => "Words",
            Self::General => "General",
        }
    }
}

pub(super) struct Navigation {
    pub(super) page: Page,
    pub(super) scroll: ScrollHandle,
    tabs: [FocusHandle; 3],
}

impl Navigation {
    pub(super) fn new(cx: &Context<'_, Settings>) -> Self {
        Self {
            page: Page::Dictation,
            scroll: ScrollHandle::new(),
            tabs: Page::ALL.map(|_| cx.focus_handle().tab_stop(true)),
        }
    }

    pub(super) const fn focus(&self, page: Page) -> &FocusHandle {
        match page {
            Page::Dictation => &self.tabs[0],
            Page::Words => &self.tabs[1],
            Page::General => &self.tabs[2],
        }
    }
}

impl Settings {
    pub(super) fn open_page(&mut self, page: Page, window: &mut Window) {
        if self.navigation.page != page {
            self.practice.stop();
            self.navigation.page = page;
            self.navigation.scroll.set_offset(point(px(0.0), px(0.0)));
        }
        self.navigation.focus(page).focus(window);
    }
}
