//! Small checked readers for ISO BMFF metadata. Neither errors nor Debug expose source bytes.

use super::MovieError;

pub(super) struct Atom {
    pub kind: [u8; 4],
    pub payload: u64,
    pub end: u64,
}

pub(super) fn require(valid: bool, message: &'static str) -> Result<(), MovieError> {
    if valid {
        Ok(())
    } else {
        Err(MovieError::Invalid(message))
    }
}

pub(super) struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    pub(super) const fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    pub(super) const fn empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) const fn rest(&self) -> &'a [u8] {
        self.0
    }

    pub(super) fn take(&mut self, length: usize) -> Result<&'a [u8], MovieError> {
        let (value, rest) = self
            .0
            .split_at_checked(length)
            .ok_or(MovieError::Invalid("Movie metadata is incomplete"))?;
        self.0 = rest;
        Ok(value)
    }

    pub(super) fn skip(&mut self, length: usize) -> Result<(), MovieError> {
        self.take(length).map(|_| ())
    }

    pub(super) fn kind(&mut self) -> Result<[u8; 4], MovieError> {
        self.take(4)?
            .try_into()
            .map_err(|_length| MovieError::Invalid("Movie box type is incomplete"))
    }

    pub(super) fn word(&mut self) -> Result<u32, MovieError> {
        Ok(u32::from_be_bytes(self.kind()?))
    }

    pub(super) fn signed(&mut self) -> Result<i32, MovieError> {
        Ok(i32::from_be_bytes(self.kind()?))
    }

    pub(super) fn wide(&mut self) -> Result<u64, MovieError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(
            |_length| MovieError::Invalid("Movie wide field is incomplete"),
        )?))
    }

    pub(super) fn short(&mut self) -> Result<u16, MovieError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().map_err(
            |_length| MovieError::Invalid("Movie short field is incomplete"),
        )?))
    }

    pub(super) fn byte(&mut self) -> Result<u8, MovieError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(MovieError::Invalid("Movie byte field is incomplete"))
    }
}

type Child<'a> = ([u8; 4], &'a [u8]);

pub(super) fn children(mut bytes: &[u8]) -> Result<Vec<Child<'_>>, MovieError> {
    let mut found = Vec::new();
    while !bytes.is_empty() {
        require(
            found.len() < 64,
            "Movie metadata contains too many child boxes",
        )?;
        let mut fields = Fields::new(bytes);
        let short = fields.word()?;
        let kind = fields.kind()?;
        let (length, header) = if short == 1 {
            (fields.wide()?, 16_u64)
        } else {
            (u64::from(short), 8)
        };
        let payload = length
            .checked_sub(header)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(MovieError::Invalid("Movie child box length is invalid"))?;
        found.push((kind, fields.take(payload)?));
        bytes = fields.rest();
    }
    Ok(found)
}

pub(super) fn child<'a>(boxes: &[Child<'a>], kind: [u8; 4]) -> Result<&'a [u8], MovieError> {
    let mut matching = boxes.iter().filter(|(candidate, _)| *candidate == kind);
    let (_, bytes) = matching.next().ok_or(MovieError::Invalid(
        "Movie metadata is missing a required box",
    ))?;
    require(
        matching.next().is_none(),
        "Movie metadata repeats a required box",
    )?;
    Ok(bytes)
}
