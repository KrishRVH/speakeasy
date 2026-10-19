//! Index ranges stored as `[start, end]`, like the manifest's other pairs.

use std::ops::Range;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

pub(crate) fn serialize<S: Serializer>(
    range: &Range<usize>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    [range.start, range.end].serialize(serializer)
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Range<usize>, D::Error> {
    let [start, end] = <[usize; 2]>::deserialize(deserializer)?;
    if start > end {
        return Err(D::Error::custom("a range cannot start after it ends"));
    }
    Ok(start..end)
}
