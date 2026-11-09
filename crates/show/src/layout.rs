use crate::{Moment, Point, Rect, recording::spaces};

pub const MOST_WINDOWS: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub at: Moment,
    pub windows: Vec<Window>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub id: u32,
    pub app: u32,
    pub layer: Layer,
    pub bounds: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    Normal,
    Floating,
    Menu,
    Bar,
}

impl Layout {
    pub(crate) fn valid(&self) -> bool {
        self.windows.len() <= MOST_WINDOWS
            && self
                .windows
                .iter()
                .all(|window| spaces::valid(window.bounds))
    }

    pub(crate) fn at(layouts: &[Self], moment: Moment) -> Option<&Self> {
        let after = layouts.partition_point(|layout| layout.at <= moment);
        layouts.get(after.saturating_sub(1))
    }

    pub(crate) fn owner(&self, point: Point) -> Option<&Window> {
        self.windows
            .iter()
            .filter(|window| window.bounds.contains(point))
            .find_map(|hit| match hit.layer {
                Layer::Normal | Layer::Floating => Some(hit),
                Layer::Menu => self.front_of(hit.app),
                Layer::Bar => self.front(),
            })
    }

    pub(crate) fn front(&self) -> Option<&Window> {
        self.windows
            .iter()
            .find(|window| window.layer == Layer::Normal)
    }

    pub(crate) fn find(&self, id: u32) -> Option<&Window> {
        self.windows.iter().find(|window| window.id == id)
    }

    pub(crate) fn ahead_of(&self, window: &Window) -> impl Iterator<Item = &Window> {
        let position = self
            .windows
            .iter()
            .position(|other| other.id == window.id)
            .unwrap_or(0);
        let (app, bounds) = (window.app, window.bounds);
        self.windows
            .iter()
            .take(position)
            .filter(move |other| other.app == app)
            .filter(move |other| match other.layer {
                Layer::Menu => true,
                Layer::Normal | Layer::Floating => other.bounds.intersection(bounds).area() > 0.0,
                Layer::Bar => false,
            })
    }

    fn front_of(&self, app: u32) -> Option<&Window> {
        self.windows.iter().find(|window| {
            window.app == app && matches!(window.layer, Layer::Normal | Layer::Floating)
        })
    }
}
