use super::{
    MovieError,
    atoms::{Fields, child, children, require},
};
use crate::MAX_FRAME_EDGE;

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
    let movie_timescale = versioned(
        child(&boxes, *b"mvhd")?.bytes,
        [100, 112],
        "Movie header is invalid",
    )?
    .word()?;
    require(movie_timescale != 0, "Movie header timescale is zero")?;
    require(
        boxes.iter().filter(|part| part.kind == *b"trak").count() == 1,
        "Movie must have one silent video track",
    )?;
    let track = children(child(&boxes, *b"trak")?.bytes)?;
    require(
        track.iter().all(|part| part.kind != *b"edts"),
        "Movie edit lists are unsupported",
    )?;
    let track_header = child(&track, *b"tkhd")?.bytes;
    let id = versioned(track_header, [84, 96], "Movie track header is invalid")?.word()?;
    require(id != 0, "Movie track identity is zero")?;
    let media = children(child(&track, *b"mdia")?.bytes)?;
    let mut handler = Fields::new(child(&media, *b"hdlr")?.bytes);
    handler.skip(8)?;
    require(
        handler.kind()? == *b"vide",
        "Movie contains a non-video track",
    )?;
    let timescale = versioned(
        child(&media, *b"mdhd")?.bytes,
        [24, 36],
        "Movie media header is invalid",
    )?
    .word()?;
    require(
        (1..=1_000_000_000).contains(&timescale),
        "Movie timescale is invalid",
    )?;
    let media = children(child(&media, *b"minf")?.bytes)?;
    let references = children(child(&media, *b"dinf")?.bytes)?;
    let mut reference = Fields::new(child(&references, *b"dref")?.bytes);
    require(
        reference.word()? == 0 && reference.word()? == 1,
        "Movie references external media",
    )?;
    let reference = children(reference.rest())?;
    require(
        reference.len() == 1 && child(&reference, *b"url ")?.bytes == [0, 0, 0, 1],
        "Movie references external media",
    )?;
    let samples = children(child(&media, *b"stbl")?.bytes)?;
    for kind in [b"stts", b"stsc", b"stco"] {
        let mut table = Fields::new(child(&samples, *kind)?.bytes);
        require(
            table.word()? == 0 && table.word()? == 0 && table.empty(),
            "Movie initialization contains samples",
        )?;
    }
    let mut sizes = Fields::new(child(&samples, *b"stsz")?.bytes);
    require(
        sizes.word()? == 0 && sizes.word()? == 0 && sizes.word()? == 0 && sizes.empty(),
        "Movie initialization contains sample sizes",
    )?;
    let mut descriptions = Fields::new(child(&samples, *b"stsd")?.bytes);
    require(
        descriptions.word()? == 0 && descriptions.word()? == 1,
        "Movie has multiple sample descriptions",
    )?;
    let description = children(descriptions.rest())?;
    require(
        description.len() == 1,
        "Movie has multiple codec configurations",
    )?;
    let mut avc = Fields::new(child(&description, *b"avc1")?.bytes);
    avc.skip(6)?;
    require(avc.short()? == 1, "Movie sample uses an external reference")?;
    avc.skip(16)?;
    let size = [u32::from(avc.short()?), u32::from(avc.short()?)];
    require(
        size.into_iter()
            .all(|edge| (2..=MAX_FRAME_EDGE).contains(&edge) && edge.is_multiple_of(2)),
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
    configuration(child(&codec, *b"avcC")?.bytes)?;
    let extends = children(child(&boxes, *b"mvex")?.bytes)?;
    require(
        extends.iter().filter(|part| part.kind == *b"trex").count() == 1,
        "Movie has multiple fragment defaults",
    )?;
    let mut defaults = Fields::new(child(&extends, *b"trex")?.bytes);
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

fn versioned<'a>(
    bytes: &'a [u8],
    [short, long]: [usize; 2],
    invalid: &'static str,
) -> Result<Fields<'a>, MovieError> {
    let mut fields = Fields::new(bytes);
    let times = match fields.word()? >> 24 {
        0 if bytes.len() == short => 8,
        1 if bytes.len() == long => 16,
        _ => return Err(MovieError::Invalid(invalid)),
    };
    fields.skip(times)?;
    Ok(fields)
}

fn configuration(bytes: &[u8]) -> Result<(), MovieError> {
    let mut fields = Fields::new(bytes);
    let [version, profile, _compatibility, _level] = fields.array()?;
    require(
        version == 1,
        "Movie AVC configuration version is unsupported",
    )?;
    let [lengths, sequences] = fields.array()?;
    require(
        lengths & 0xFC == 0xFC && lengths & 0x03 != 0x02 && sequences & 0xE0 == 0xE0,
        "Movie AVC configuration flags are invalid",
    )?;
    parameters(&mut fields, sequences & 0x1F)?;
    let pictures = fields.byte()?;
    parameters(&mut fields, pictures)?;
    if !fields.empty() {
        require(
            [100, 110, 122, 144].contains(&profile),
            "Movie AVC extension is unsupported",
        )?;
        let [chroma, luma, chroma_depth, count] = fields.array()?;
        require(
            chroma & 0xFC == 0xFC && luma & 0xF8 == 0xF8 && chroma_depth & 0xF8 == 0xF8,
            "Movie AVC extension flags are invalid",
        )?;
        for _ in 0..count {
            parameter(&mut fields)?;
        }
    }
    require(fields.empty(), "Movie AVC configuration has trailing bytes")
}

fn parameters(fields: &mut Fields<'_>, count: u8) -> Result<(), MovieError> {
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
