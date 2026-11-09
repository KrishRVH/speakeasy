#[expect(
    clippy::cast_possible_truncation,
    reason = "Channels round to eight bits; out-of-range values saturate"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "Negative channel values saturate to zero"
)]
pub(crate) fn quantize(value: f32) -> u8 {
    value.round() as u8
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Pixel measures round to whole pixels and saturate at the integer range"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "Negative measures saturate to zero pixels"
)]
pub(crate) fn pixels(value: f32) -> u32 {
    value.round() as u32
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Values floor to a whole count or index and saturate at the usize range"
)]
#[expect(clippy::cast_sign_loss, reason = "Negative values saturate to zero")]
pub(crate) fn count_floor(value: f32) -> usize {
    value.floor() as usize
}

pub(crate) fn count_ceil(value: f32) -> usize {
    count_floor(value.ceil())
}

pub(crate) fn count(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
