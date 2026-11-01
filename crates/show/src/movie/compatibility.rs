use super::{MovieError, require};

#[derive(Clone, Debug, Default)]
pub struct MovieCompatibility {
    pub(super) headers: Vec<u64>,
}

impl MovieCompatibility {
    pub fn apply(&self, at: u64, bytes: &mut [u8]) -> Result<(), MovieError> {
        require(
            u64::try_from(bytes.len())
                .ok()
                .and_then(|length| at.checked_add(length))
                .is_some(),
            "Movie read range overflows",
        )?;
        let first = self.headers.partition_point(|offset| *offset < at);
        for &offset in self.headers.iter().skip(first) {
            let Some(flag) = offset
                .checked_sub(at)
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| bytes.get_mut(index))
            else {
                break;
            };
            require(matches!(*flag, 1 | 17), "Movie fragment header changed")?;
            *flag |= 16;
        }
        Ok(())
    }
}
