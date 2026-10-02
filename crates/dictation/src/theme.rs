//! Color themes for Settings, the pill, and the tray, and the blending they share.

use serde::{Deserialize, Serialize};

/// A selectable color theme, saved by its snake-case name.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Jet & Champagne, the default.
    #[default]
    Jet,
    /// Emerald Lounge.
    Emerald,
    /// Iris.
    Iris,
    /// Midnight Chrome.
    Chrome,
}

impl Theme {
    /// The name Settings shows.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Jet => "Jet & Champagne",
            Self::Emerald => "Emerald Lounge",
            Self::Iris => "Iris",
            Self::Chrome => "Midnight Chrome",
        }
    }

    /// The theme after this one in Settings' cycle.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Jet => Self::Emerald,
            Self::Emerald => Self::Iris,
            Self::Iris => Self::Chrome,
            Self::Chrome => Self::Jet,
        }
    }

    /// The colors every surface paints with.
    #[must_use]
    pub fn palette(self) -> &'static Palette {
        match self {
            Self::Jet => &Palette {
                room: 0x0A_0A_0B,
                door: 0x13_12_11,
                panel: 0x15_14_13,
                raise: 0x22_1F_1B,
                ink: 0xF4_EF_E6,
                muted: 0xAD_A5_97,
                lamp: 0xE6_CD_96,
                live: 0xFF_4D_3D,
                warn: 0xFF_CB_4A,
            },
            Self::Emerald => &Palette {
                room: 0x07_12_0F,
                // Not 0x0C_1A_16: clippy reads a trailing `_16` as a mistyped suffix.
                door: 0x000C_1A16,
                panel: 0x0F_1F_1A,
                raise: 0x18_30_28,
                ink: 0xE8_EF_E9,
                muted: 0x97_AE_A1,
                lamp: 0xD9_B4_6E,
                live: 0xFF_5B_3A,
                warn: 0xFF_D0_45,
            },
            Self::Iris => &Palette {
                room: 0x11_0D_14,
                door: 0x19_13_1E,
                panel: 0x1A_14_20,
                raise: 0x28_1F_30,
                ink: 0xF2_EA_F0,
                muted: 0xB4_A5_B6,
                lamp: 0xC4_A7_E4,
                live: 0xFF_4D_63,
                warn: 0xFF_C2_4E,
            },
            Self::Chrome => &Palette {
                room: 0x09_0D_17,
                door: 0x0F_15_24,
                panel: 0x11_18_29,
                raise: 0x1B_24_38,
                ink: 0xEA_F0_F7,
                muted: 0x9E_AB_BD,
                lamp: 0xC5_D0_DE,
                live: 0xFF_4A_4A,
                warn: 0xF7_B8_4A,
            },
        }
    }
}

/// A theme's `0xRRGGBB` colors, by role.
pub struct Palette {
    /// The Settings window background.
    pub room: u32,
    /// The pill's capsule.
    pub door: u32,
    /// Settings panels.
    pub panel: u32,
    /// Raised controls such as buttons.
    pub raise: u32,
    /// Primary text and strokes.
    pub ink: u32,
    /// Secondary text and symbols.
    pub muted: u32,
    /// The brand accent; it never signals recording.
    pub lamp: u32,
    /// Red, reserved for open capture.
    pub live: u32,
    /// Attention and notices.
    pub warn: u32,
}

impl Palette {
    /// `muted` faded toward `room`, for the quietest text and idle marks.
    #[must_use]
    pub fn faint(&self) -> u32 {
        mix(self.room, self.muted, 0.6)
    }
}

/// Linear blend of two `0xRRGGBB` colors; `weight` in `0.0..=1.0` moves from `from` to `to`.
#[must_use]
pub fn mix(from: u32, to: u32, weight: f32) -> u32 {
    [16, 8, 0].into_iter().fold(0, |mixed, shift| {
        let channel = |color: u32| ((color >> shift) & 0xFF) as f32;
        let (start, end) = (channel(from), channel(to));
        mixed | (u32::from(quantize((end - start).mul_add(weight, start))) << shift)
    })
}

/// Rounds a color channel or opacity in `0.0..=255.0` to eight bits.
#[expect(
    clippy::cast_possible_truncation,
    reason = "Channels deliberately round to eight bits; out-of-range values saturate"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "Negative values saturate to zero, which no in-range channel produces"
)]
#[must_use]
pub fn quantize(value: f32) -> u8 {
    value.round() as u8
}
