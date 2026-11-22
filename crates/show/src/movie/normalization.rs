use super::{MovieError, require};

const UNDECLARED: u8 = 0x01;
const DECLARED: u8 = 0x11;

/// Offset-preserving fix for the zero word Media Foundation appends to each `tfhd`.
///
/// Setting the default-sample-size flag declares that word, so the Mac reader can use the saved
/// bytes directly. Only one flag byte per fragment changes; picture bytes, sample timing, and
/// file offsets must stay intact. `fragment` records a header only when `tfhd` has no other
/// optional field and every `trun` carries its own sample sizes, so the declared zero is unused.
#[derive(Clone, Debug, Default)]
pub struct MovieNormalization {
    pub(super) headers: Vec<u64>,
}

impl MovieNormalization {
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
            require(
                matches!(*flag, UNDECLARED | DECLARED),
                "Movie fragment header changed",
            )?;
            *flag = DECLARED;
        }
        Ok(())
    }
}
