use serde::{Deserialize, Serialize};

/// Settings, pill, and tray colors. Red (`live`) is reserved for open capture;
/// `lamp` is the brand accent and never signals recording.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Theme {
    #[default]
    Jet,
    Emerald,
    Iris,
    Chrome,
}

pub(crate) struct Palette {
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
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Jet => "Jet & Champagne",
            Self::Emerald => "Emerald Lounge",
            Self::Iris => "Iris",
            Self::Chrome => "Midnight Chrome",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Jet => Self::Emerald,
            Self::Emerald => Self::Iris,
            Self::Iris => Self::Chrome,
            Self::Chrome => Self::Jet,
        }
    }

    pub(crate) fn palette(self) -> &'static Palette {
        match self {
            Self::Jet => &Palette {
                room: 0x0a_0a_0b,
                door: 0x13_12_11,
                panel: 0x15_14_13,
                raise: 0x22_1f_1b,
                ink: 0xf4_ef_e6,
                muted: 0xad_a5_97,
                lamp: 0xe6_cd_96,
                live: 0xff_4d_3d,
                warn: 0xff_cb_4a,
            },
            Self::Emerald => &Palette {
                room: 0x07_12_0f,
                door: 0x000c_1a16,
                panel: 0x0f_1f_1a,
                raise: 0x18_30_28,
                ink: 0xe8_ef_e9,
                muted: 0x97_ae_a1,
                lamp: 0xd9_b4_6e,
                live: 0xff_5b_3a,
                warn: 0xff_d0_45,
            },
            Self::Iris => &Palette {
                room: 0x11_0d_14,
                door: 0x19_13_1e,
                panel: 0x1a_14_20,
                raise: 0x28_1f_30,
                ink: 0xf2_ea_f0,
                muted: 0xb4_a5_b6,
                lamp: 0xc4_a7_e4,
                live: 0xff_4d_63,
                warn: 0xff_c2_4e,
            },
            Self::Chrome => &Palette {
                room: 0x09_0d_17,
                door: 0x0f_15_24,
                panel: 0x11_18_29,
                raise: 0x1b_24_38,
                ink: 0xea_f0_f7,
                muted: 0x9e_ab_bd,
                lamp: 0xc5_d0_de,
                live: 0xff_4a_4a,
                warn: 0xf7_b8_4a,
            },
        }
    }
}

/// Linear blend of two `0xRRGGBB` colors with a weight in `0.0..=1.0`.
pub(crate) fn mix(a: u32, b: u32, t: f32) -> u32 {
    [16, 8, 0].into_iter().fold(0, |color, shift| {
        let from = ((a >> shift) & 0xff) as f32;
        let to = ((b >> shift) & 0xff) as f32;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Interpolated eight-bit channels deliberately round to whole color components"
        )]
        #[expect(
            clippy::cast_sign_loss,
            reason = "Eight-bit source channels and a weight in 0..=1 keep the blend nonnegative"
        )]
        let channel = (to - from).mul_add(t, from).round() as u32;
        color | (channel << shift)
    })
}

/// A `0xRRGGBB` color with an opacity, for GPUI fills and borders.
pub(crate) fn alpha(color: u32, opacity: f32) -> gpui::Rgba {
    gpui::Rgba {
        a: opacity,
        ..gpui::rgb(color)
    }
}
