use super::{MovieError, require};

/// Validated, offset-preserving corrections for native MP4 interoperability.
///
/// Media Foundation appends an undeclared zero word to fragment headers. A correction declares
/// that word as the default sample size only when every run supplies its own sample sizes and no
/// later optional header field can change meaning. Pictures, timestamps and byte offsets stay intact.
#[derive(Clone, Debug, Default)]
pub struct MovieCompatibility {
    pub(super) headers: Vec<u64>,
}

impl MovieCompatibility {
    /// Corrects a byte range read from the same file used for inspection. Supports arbitrary read
    /// boundaries and repeated application; the source file need not be modified.
    ///
    /// # Errors
    /// Fails if a corrected header byte no longer matches the inspected format.
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
