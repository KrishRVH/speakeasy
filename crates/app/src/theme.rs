use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Theme {
    #[default]
    Jet,
    Emerald,
    Iris,
    Chrome,
}

impl Theme {
    pub(crate) const ALL: [Self; 4] = [Self::Jet, Self::Emerald, Self::Iris, Self::Chrome];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Jet => "Jet & Champagne",
            Self::Emerald => "Emerald Lounge",
            Self::Iris => "Iris",
            Self::Chrome => "Midnight Chrome",
        }
    }

    pub(crate) const fn palette(self) -> &'static Palette {
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
                door: 0x0C_1A_16_u32,
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

impl Palette {
    pub(crate) fn line(&self) -> gpui::Rgba {
        alpha(self.ink, 0.09)
    }

    pub(crate) fn edge(&self) -> gpui::Rgba {
        alpha(self.ink, 0.16)
    }

    pub(crate) fn hover(&self) -> gpui::Rgba {
        alpha(self.ink, 0.07)
    }
}

pub(crate) fn mix(from: u32, to: u32, weight: f32) -> u32 {
    let mut mixed = from.to_be_bytes();
    for (channel, end) in mixed.iter_mut().zip(to.to_be_bytes()) {
        let start = f32::from(*channel);
        *channel = quantize(start + (f32::from(end) - start) * weight);
    }
    u32::from_be_bytes(mixed)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Channels deliberately round to eight bits; out-of-range values saturate"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "Negative values saturate to zero, which no in-range channel produces"
)]
pub(crate) fn quantize(value: f32) -> u8 {
    value.round() as u8
}

pub(crate) fn alpha(color: u32, opacity: f32) -> gpui::Rgba {
    gpui::rgb(color).alpha(opacity)
}
