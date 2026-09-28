use serde::{Deserialize, Serialize};

/// Settings, pill, and tray colors. Red (`live`) is reserved for open capture;
/// `lamp` is the brand accent and never signals recording.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Jet,
    Emerald,
    Iris,
    Chrome,
}

pub struct Palette {
    pub room: u32,
    pub door: u32,
    pub panel: u32,
    pub raise: u32,
    pub ink: u32,
    pub muted: u32,
    pub lamp: u32,
    pub live: u32,
    pub warn: u32,
}

impl Theme {
    pub fn name(self) -> &'static str {
        match self {
            Self::Jet => "Jet & Champagne",
            Self::Emerald => "Emerald Lounge",
            Self::Iris => "Iris",
            Self::Chrome => "Midnight Chrome",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Jet => Self::Emerald,
            Self::Emerald => Self::Iris,
            Self::Iris => Self::Chrome,
            Self::Chrome => Self::Jet,
        }
    }

    pub fn palette(self) -> &'static Palette {
        match self {
            Self::Jet => &Palette {
                room: 0x0a0a0b,
                door: 0x131211,
                panel: 0x151413,
                raise: 0x221f1b,
                ink: 0xf4efe6,
                muted: 0xada597,
                lamp: 0xe6cd96,
                live: 0xff4d3d,
                warn: 0xffcb4a,
            },
            Self::Emerald => &Palette {
                room: 0x07120f,
                door: 0x0c1a16,
                panel: 0x0f1f1a,
                raise: 0x183028,
                ink: 0xe8efe9,
                muted: 0x97aea1,
                lamp: 0xd9b46e,
                live: 0xff5b3a,
                warn: 0xffd045,
            },
            Self::Iris => &Palette {
                room: 0x110d14,
                door: 0x19131e,
                panel: 0x1a1420,
                raise: 0x281f30,
                ink: 0xf2eaf0,
                muted: 0xb4a5b6,
                lamp: 0xc4a7e4,
                live: 0xff4d63,
                warn: 0xffc24e,
            },
            Self::Chrome => &Palette {
                room: 0x090d17,
                door: 0x0f1524,
                panel: 0x111829,
                raise: 0x1b2438,
                ink: 0xeaf0f7,
                muted: 0x9eabbd,
                lamp: 0xc5d0de,
                live: 0xff4a4a,
                warn: 0xf7b84a,
            },
        }
    }
}

/// Linear blend of two `0xRRGGBB` colors, `t` of the way from `a` to `b`.
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    [16, 8, 0].into_iter().fold(0, |color, shift| {
        let from = ((a >> shift) & 0xff) as f32;
        let to = ((b >> shift) & 0xff) as f32;
        color | (((from + (to - from) * t).round() as u32) << shift)
    })
}

/// A `0xRRGGBB` color with an opacity, for GPUI fills and borders.
pub fn alpha(color: u32, opacity: f32) -> gpui::Rgba {
    gpui::Rgba {
        a: opacity,
        ..gpui::rgb(color)
    }
}
