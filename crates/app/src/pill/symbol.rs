use gpui::{prelude::*, px, rgb, svg};

use crate::icons;

#[derive(Clone, Copy)]
pub(super) enum Symbol {
    Microphone,
    Lock,
    Screen,
    Check,
}

impl Symbol {
    const fn asset(self) -> &'static str {
        match self {
            Self::Microphone => icons::MICROPHONE,
            Self::Lock => icons::LOCK,
            Self::Screen => icons::SCREEN,
            Self::Check => icons::CHECK,
        }
    }

    pub(super) fn element(self, color: u32) -> impl IntoElement {
        svg()
            .path(self.asset())
            .size(px(16.0))
            .flex_shrink_0()
            .text_color(rgb(color))
    }
}
