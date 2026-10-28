//! The crate's float-to-integer conversions, from channel values to bytes and from layout
//! coordinates to pixel counts and indices, and the widening of pixel dimensions to index bounds.
//!
//! Float-to-integer `as` casts saturate at the target's bounds and map NaN to zero, which is the
//! intended behavior for every caller: geometry that strays outside an image clamps to its edge.

/// Rounds a channel value in `0.0..=255.0` to eight bits; strays clamp to the range.
#[expect(
    clippy::cast_possible_truncation,
    reason = "Channels deliberately round to eight bits; out-of-range values saturate"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "Negative channel values saturate to zero"
)]
pub(crate) fn quantize(value: f32) -> u8 {
    value.round() as u8
}

/// Rounds a pixel measure to a whole count; negative values and NaN become zero.
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

/// Rounds a pixel position down to a whole index; negative values and NaN become zero.
#[expect(
    clippy::cast_possible_truncation,
    reason = "Positions truncate to the pixel they fall in and saturate at the index range"
)]
#[expect(
    clippy::cast_sign_loss,
    reason = "Positions before the first pixel clamp to it"
)]
pub(crate) fn count_floor(value: f32) -> usize {
    value.floor() as usize
}

/// Rounds a pixel position up to a whole index; negative values and NaN become zero.
pub(crate) fn count_ceil(value: f32) -> usize {
    count_floor(value.ceil())
}

/// Widens a pixel dimension to an index bound.
pub(crate) fn count(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
