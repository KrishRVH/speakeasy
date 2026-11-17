use std::num::NonZeroU64;

#[derive(Clone, Copy)]
pub(crate) enum Progress {
    Step(&'static str),
    Bytes {
        step: &'static str,
        done: u64,
        total: NonZeroU64,
    },
}

impl Progress {
    pub(crate) fn bytes(step: &'static str, done: u64, total: u64) -> Self {
        NonZeroU64::new(total).map_or(Self::Step(step), |total| Self::Bytes { step, done, total })
    }
}
