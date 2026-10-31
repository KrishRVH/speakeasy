//! The supported movie initialization has one AVC video track and no external media references.

use super::{
    MovieError,
    atoms::{Fields, child, children, require},
};

pub(super) struct Track {
    pub id: u32,
    pub size: [u32; 2],
    pub timescale: u32,
    pub duration: u32,
    pub sample_size: u32,
    pub flags: u32,
}

pub(super) fn read(bytes: &[u8]) -> Result<Track, MovieError> {
    let boxes = children(bytes)?;
    let mut movie = Fields::new(child(&boxes, *b"mvhd")?);
    let version = movie.word()? >> 24;
    require(
        (version == 0 && movie.rest().len() == 96) || (version == 1 && movie.rest().len() == 108),
        "Movie header size is invalid",
    )?;
    movie.skip(if version == 0 { 8 } else { 16 })?;
    require(movie.word()? != 0, "Movie header timescale is zero")?;
    require(
        boxes.iter().filter(|(kind, _)| kind == b"trak").count() == 1,
        "Movie must have one silent video track",
    )?;
    let track = children(child(&boxes, *b"trak")?)?;
    require(
        track.iter().all(|(kind, _)| kind != b"edts"),
        "Movie edit lists are unsupported",
    )?;
    let track_header = child(&track, *b"tkhd")?;
    let mut header = Fields::new(track_header);
    match header.word()? >> 24 {
        0 => {
            require(
                track_header.len() == 84,
                "Movie track header size is invalid",
            )?;
            header.skip(8)?;
        },
        1 => {
            require(
                track_header.len() == 96,
                "Movie track header size is invalid",
            )?;
            header.skip(16)?;
        },
        _ => {
            return Err(MovieError::Invalid(
                "Movie track header version is unsupported",
            ));
        },
    }
    let id = header.word()?;
    require(id != 0, "Movie track identity is zero")?;
    let media = children(child(&track, *b"mdia")?)?;
    let mut handler = Fields::new(child(&media, *b"hdlr")?);
    handler.skip(8)?;
    require(
        handler.kind()? == *b"vide",
        "Movie contains a non-video track",
    )?;
    let media_header = child(&media, *b"mdhd")?;
    let mut header = Fields::new(media_header);
    match header.word()? >> 24 {
        0 => {
            require(
                media_header.len() == 24,
                "Movie media header size is invalid",
            )?;
            header.skip(8)?;
        },
        1 => {
            require(
                media_header.len() == 36,
                "Movie media header size is invalid",
            )?;
            header.skip(16)?;
        },
        _ => {
            return Err(MovieError::Invalid(
                "Movie media header version is unsupported",
            ));
        },
    }
    let timescale = header.word()?;
    require(
        (1..=1_000_000_000).contains(&timescale),
        "Movie timescale is invalid",
    )?;
    let media = children(child(&media, *b"minf")?)?;
    let references = children(child(&media, *b"dinf")?)?;
    let mut reference = Fields::new(child(&references, *b"dref")?);
    require(
        reference.word()? == 0 && reference.word()? == 1,
        "Movie references external media",
    )?;
    let reference = children(reference.rest())?;
    require(
        reference.len() == 1 && child(&reference, *b"url ")? == [0, 0, 0, 1],
        "Movie references external media",
    )?;
    let samples = children(child(&media, *b"stbl")?)?;
    for kind in [b"stts", b"stsc", b"stco"] {
        let mut table = Fields::new(child(&samples, *kind)?);
        require(
            table.word()? == 0 && table.word()? == 0 && table.empty(),
            "Movie initialization contains samples",
        )?;
    }
    let mut sizes = Fields::new(child(&samples, *b"stsz")?);
    require(
        sizes.word()? == 0 && sizes.word()? == 0 && sizes.word()? == 0 && sizes.empty(),
        "Movie initialization contains sample sizes",
    )?;
    let mut descriptions = Fields::new(child(&samples, *b"stsd")?);
    require(
        descriptions.word()? == 0 && descriptions.word()? == 1,
        "Movie has multiple sample descriptions",
    )?;
    let description = children(descriptions.rest())?;
    require(
        description.len() == 1,
        "Movie has multiple codec configurations",
    )?;
    let mut avc = Fields::new(child(&description, *b"avc1")?);
    avc.skip(6)?;
    require(avc.short()? == 1, "Movie sample uses an external reference")?;
    avc.skip(16)?;
    let size = [u32::from(avc.short()?), u32::from(avc.short()?)];
    require(
        size.into_iter().all(|edge| {
            (2..=super::super::MAX_FRAME_EDGE).contains(&edge) && edge.is_multiple_of(2)
        }),
        "Movie canvas is invalid",
    )?;
    let mut display = Fields::new(
        track_header
            .get(track_header.len().saturating_sub(8)..)
            .ok_or(MovieError::Invalid("Movie display size is missing"))?,
    );
    require(
        [display.word()?, display.word()?] == size.map(|edge| edge << 16),
        "Movie display and encoded sizes differ",
    )?;
    avc.skip(50)?;
    let codec = children(avc.rest())?;
    let config = child(&codec, *b"avcC")?;
    configuration(config)?;
    let extends = children(child(&boxes, *b"mvex")?)?;
    require(
        extends.iter().filter(|(kind, _)| kind == b"trex").count() == 1,
        "Movie has multiple fragment defaults",
    )?;
    let mut defaults = Fields::new(child(&extends, *b"trex")?);
    require(
        defaults.word()? == 0 && defaults.word()? == id && defaults.word()? == 1,
        "Movie fragment defaults do not match its track",
    )?;
    let result = Track {
        id,
        size,
        timescale,
        duration: defaults.word()?,
        sample_size: defaults.word()?,
        flags: defaults.word()?,
    };
    require(
        defaults.empty(),
        "Movie fragment defaults have trailing data",
    )?;
    Ok(result)
}

/// Validate the `AVCDecoderConfigurationRecord` envelopes, not the encoded parameter-set syntax.
fn configuration(bytes: &[u8]) -> Result<(), MovieError> {
    let mut fields = Fields::new(bytes);
    let header = fields.word()?;
    require(
        header >> 24 == 1,
        "Movie AVC configuration version is unsupported",
    )?;
    let profile = (header >> 16) & 0xFF;
    let counts = fields.short()?;
    require(
        counts & 0xFCE0 == 0xFCE0 && (counts >> 8) & 3 != 2,
        "Movie AVC configuration flags are invalid",
    )?;
    parameters(&mut fields, usize::from(counts & 31))?;
    let pictures = fields.byte()?;
    parameters(&mut fields, usize::from(pictures))?;
    if !fields.empty() {
        require(
            [100, 110, 122, 144].contains(&profile),
            "Movie AVC extension is unsupported",
        )?;
        let chroma = fields.byte()?;
        let luma = fields.byte()?;
        let chroma_depth = fields.byte()?;
        require(
            chroma & 0xFC == 0xFC && luma & 0xF8 == 0xF8 && chroma_depth & 0xF8 == 0xF8,
            "Movie AVC extension flags are invalid",
        )?;
        let count = fields.byte()?;
        for _ in 0..count {
            parameter(&mut fields)?;
        }
    }
    require(fields.empty(), "Movie AVC configuration has trailing bytes")
}

fn parameters(fields: &mut Fields<'_>, count: usize) -> Result<(), MovieError> {
    require(count > 0, "Movie AVC parameters are missing")?;
    for _ in 0..count {
        parameter(fields)?;
    }
    Ok(())
}

fn parameter(fields: &mut Fields<'_>) -> Result<(), MovieError> {
    let length = fields.short()?;
    require(length > 0, "Movie AVC parameter is empty")?;
    fields.skip(usize::from(length))
}
