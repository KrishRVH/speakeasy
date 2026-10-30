//! Framed binary records with bounded cursors and content-free failures.

use super::SourceError;

pub(super) fn require(condition: bool, message: &'static str) -> Result<(), SourceError> {
    if condition {
        Ok(())
    } else {
        Err(SourceError(message))
    }
}

pub(super) fn emit_record<E: From<SourceError>>(
    kind: u8,
    data: &[u8],
    emit: &mut impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    let length = u32::try_from(data.len())
        .map_err(|_out_of_range| SourceError("source record is too large"))?;
    emit(&length.to_le_bytes())?;
    emit(&[kind])?;
    emit(data)?;
    let mut crc = crc32fast::Hasher::new();
    crc.update(&[kind]);
    crc.update(data);
    emit(&crc.finalize().to_le_bytes())
}

pub(super) fn payload(record: &[u8], kind: u8) -> Result<&[u8], SourceError> {
    let mut cursor = Cursor(record);
    let length = cursor.usize()?;
    let actual = cursor.take::<1>()?;
    require(actual == [kind], "unsupported source record")?;
    require(
        cursor.0.len() == length.saturating_add(4),
        "source record length is invalid",
    )?;
    let (data, checksum) = cursor.0.split_at(length);
    let expected = Cursor(checksum).word()?;
    let mut crc = crc32fast::Hasher::new();
    crc.update(&actual);
    crc.update(data);
    require(crc.finalize() == expected, "source record checksum failed")?;
    Ok(data)
}

pub(super) struct Cursor<'a>(pub(super) &'a [u8]);

impl<'a> Cursor<'a> {
    pub(super) fn take<const N: usize>(&mut self) -> Result<[u8; N], SourceError> {
        let (head, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(SourceError("source index is truncated"))?;
        self.0 = rest;
        Ok(*head)
    }

    pub(super) fn word(&mut self) -> Result<u32, SourceError> {
        self.take().map(u32::from_le_bytes)
    }

    pub(super) fn wide(&mut self) -> Result<u64, SourceError> {
        self.take().map(u64::from_le_bytes)
    }

    pub(super) fn usize(&mut self) -> Result<usize, SourceError> {
        self.word().map(crate::convert::count)
    }

    pub(super) fn count(&mut self, limit: usize, minimum: usize) -> Result<usize, SourceError> {
        let count = self.usize()?;
        require(
            count <= limit && count <= self.0.len().checked_div(minimum).unwrap_or(0),
            "source index count is invalid",
        )?;
        Ok(count)
    }

    pub(super) fn data(&mut self, length: usize) -> Result<&'a [u8], SourceError> {
        let (head, tail) = self
            .0
            .split_at_checked(length)
            .ok_or(SourceError("record is truncated"))?;
        self.0 = tail;
        Ok(head)
    }
}
