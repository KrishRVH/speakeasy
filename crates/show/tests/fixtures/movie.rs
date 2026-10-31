// Hand-built container metadata and synthetic payloads. These exercise the bounded container
// parser, not AVC decoding; native acceptance supplies actual compressed images separately.

pub(super) struct MovieFixture {
    pub bytes: Vec<u8>,
    pub initialized: usize,
    pub ends: Vec<usize>,
}

fn atom(kind: [u8; 4], body: impl AsRef<[u8]>) -> Vec<u8> {
    let body = body.as_ref();
    let mut bytes = u32::try_from(body.len().checked_add(8).unwrap())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    bytes.extend(kind);
    bytes.extend(body);
    bytes
}

fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect()
}

pub(super) fn movie() -> MovieFixture {
    let mut movie_header = words(&[0, 0, 0, 1_000]);
    movie_header.resize(100, 0);
    let mut track_header = words(&[1, 0, 0, 1]);
    track_header.resize(76, 0);
    track_header.extend(words(&[4 << 16, 2 << 16]));
    let mut media_header = words(&[0, 0, 0, 1_000]);
    media_header.resize(24, 0);
    let mut handler = words(&[0, 0]);
    handler.extend(b"vide");
    handler.resize(25, 0);
    let mut avc = vec![0; 6];
    avc.extend(1_u16.to_be_bytes());
    avc.resize(24, 0);
    avc.extend(4_u16.to_be_bytes());
    avc.extend(2_u16.to_be_bytes());
    avc.resize(78, 0);
    avc.extend(atom(
        *b"avcC",
        [1, 100, 0, 31, 255, 225, 0, 1, 103, 1, 0, 1, 104],
    ));
    let mut descriptions = words(&[0, 1]);
    descriptions.extend(atom(*b"avc1", avc));
    let mut samples = atom(*b"stsd", descriptions);
    for kind in [b"stts", b"stsc", b"stco"] {
        samples.extend(atom(*kind, words(&[0, 0])));
    }
    samples.extend(atom(*b"stsz", words(&[0, 0, 0])));
    let mut references = words(&[0, 1]);
    references.extend(atom(*b"url ", words(&[1])));
    let mut minf = atom(*b"dinf", atom(*b"dref", references));
    minf.extend(atom(*b"stbl", samples));
    let mut media = atom(*b"mdhd", media_header);
    media.extend(atom(*b"hdlr", handler));
    media.extend(atom(*b"minf", minf));
    let mut track = atom(*b"tkhd", track_header);
    track.extend(atom(*b"mdia", media));
    let mut initialization = atom(*b"mvhd", movie_header);
    initialization.extend(atom(*b"trak", track));
    initialization.extend(atom(*b"mvex", atom(*b"trex", words(&[0, 1, 1, 0, 0, 0]))));
    let mut bytes = atom(*b"ftyp", b"isom\0\0\0\0isom");
    bytes.extend(atom(*b"moov", initialization));
    let initialized = bytes.len();
    let mut ends = Vec::new();
    for sequence in 1..=2 {
        let fragment_header = atom(*b"tfhd", words(&[0x0002_0000, 1]));
        // 16 mfhd + 8 fragment_track + 16 fragment_header + 36 run + 8 fragment + 8 mdat = 92 bytes to payload.
        let run = atom(*b"trun", words(&[0x0100_0F01, 1, 92, 1_000, 6, 0, 0]));
        let mut fragment_track = fragment_header;
        fragment_track.extend(run);
        let mut fragment = atom(*b"mfhd", words(&[0, sequence]));
        fragment.extend(atom(*b"traf", fragment_track));
        bytes.extend(atom(*b"moof", fragment));
        bytes.extend(atom(*b"mdat", [0, 0, 0, 2, 0x65, 0x80]));
        ends.push(bytes.len());
    }
    MovieFixture {
        bytes,
        initialized,
        ends,
    }
}
