use std::{fs, io, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use speakeasy_show::{Manifest, Narration};

use super::{
    DISCLOSURE, Roots, protocol,
    scope::{PAGE, PATH_LIMIT},
};
use crate::show::{
    evidence::{
        Inspection,
        query::{self, Found, Query},
    },
    store::{recall, source},
};

const TEXT_LIMIT: usize = 64 << 10;
const WORD_LIMIT: usize = 512;
const TIME_LIMIT: usize = 32;
const IMAGE_LIMIT: u64 = 8 << 20;
const PIXEL_EDGE_LIMIT: u32 = 4096;
const DECODE_LIMIT: u64 = 64 << 20;
const EVIDENCE_LIMIT: usize = 1 << 20;

#[derive(Clone, Copy)]
pub(super) enum Tool {
    List,
    Read,
    Image,
}

impl Tool {
    const ALL: [Self; 3] = [Self::List, Self::Read, Self::Image];

    const fn name(self) -> &'static str {
        match self {
            Self::List => "show_list",
            Self::Read => "show_read",
            Self::Image => "show_image",
        }
    }

    pub(super) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }
}

pub(super) fn definitions() -> Value {
    let show = json!({"type": "string", "minLength": 1, "maxLength": PATH_LIMIT, "description": "Exact absolute Show path from show_list, or an unambiguous folder name within a configured root. Never latest."});
    let at = json!({"type": "string", "maxLength": TIME_LIMIT, "description": "Recording time as seconds or M:SS.mmm."});
    let positive = json!({"type": "integer", "minimum": 1});
    let phrase = json!({"type": "string", "minLength": 1, "maxLength": 2048, "description": "Phrase in recorded narration, requiring word timings; not edited summary text."});
    let annotations = json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false});
    json!([
        {"name": Tool::List.name(), "description": format!("List at most {PAGE} saved Shows inside launch-time roots, with metadata only and a continuation cursor. Roots that cannot be listed are named in unavailableRoots. {DISCLOSURE}"),
         "inputSchema": {"type": "object", "properties": {"cursor": {"type": "string", "maxLength": PATH_LIMIT}}, "additionalProperties": false}, "annotations": annotations},
        {"name": Tool::Read.name(), "description": format!("Read one exact Show's edited summary and metadata. Optionally include bounded recorded narration and one time, mark, or phrase lookup. Multiple phrase matches remain ambiguous unless match is given. Text truncation and missing or untimed narration are disclosed. {DISCLOSURE}"),
         "inputSchema": {"type": "object", "properties": {"show": show, "narration": {"type": "boolean", "default": false}, "at": at, "mark": positive, "phrase": phrase, "match": positive}, "required": ["show"], "additionalProperties": false}, "annotations": annotations},
        {"name": Tool::Image.name(), "description": format!("Read exactly one PNG, at most 8 MiB. Choose one sheet, keyframe, time, mark, or phrase (with match if ambiguous). Sheet/keyframe numbers start at 1. Time/mark/phrase reads the retained source image at or before the selection, or the first sample if capture began later; sampledMs reports its actual time and requestedMs the selection. Source images have no ink. Missing retained source is an error; no fallback or file writes. {DISCLOSURE}"),
         "inputSchema": {"type": "object", "properties": {"show": show, "sheet": positive, "keyframe": positive, "at": at, "mark": positive, "phrase": phrase, "match": positive}, "required": ["show"], "oneOf": [{"required": ["sheet"]}, {"required": ["keyframe"]}, {"required": ["at"]}, {"required": ["mark"]}, {"required": ["phrase"]}], "additionalProperties": false}, "annotations": annotations}
    ])
}

pub(super) fn call(roots: &Roots, tool: Tool, arguments: Value) -> Value {
    let result = match tool {
        Tool::List => list(roots, arguments),
        Tool::Read => read(roots, arguments),
        Tool::Image => image(roots, arguments),
    };
    match result {
        Ok(content) => json!({"content": content, "isError": false}),
        Err(message) => json!({"content": [{"type": "text", "text": message}], "isError": true}),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    cursor: Option<String>,
}

fn list(roots: &Roots, arguments: Value) -> Result<Vec<Value>, String> {
    let request: List = decode(arguments)?;
    Ok(vec![text(&roots.list(request.cursor.as_deref())?)?])
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    show: String,
    #[serde(default)]
    narration: bool,
    at: Option<String>,
    mark: Option<usize>,
    phrase: Option<String>,
    #[serde(rename = "match")]
    occurrence: Option<usize>,
}

#[derive(Serialize)]
struct ReadEvidence {
    show: Inspection,
    summary: Value,
    narration: Option<Value>,
    location: Option<Found>,
}

fn read(roots: &Roots, arguments: Value) -> Result<Vec<Value>, String> {
    let request: Read = decode(arguments)?;
    let query = lookup(request.at, request.mark, request.phrase, request.occurrence)?;
    let directory = roots.resolve(&request.show)?;
    let summary =
        recall::summary(&directory).map_err(|_private| "Cannot read that Show's edited summary")?;
    let entry = open(&directory, roots.managed())?;
    let manifest = manifest_of(&entry)?;
    let found = query
        .map(|query| query.resolve(manifest))
        .transpose()
        .map_err(|safe| safe.to_string())?;
    let content = ReadEvidence {
        show: Inspection::of(&entry),
        summary: excerpt(&summary),
        narration: request.narration.then(|| narration(manifest)),
        location: found,
    };
    Ok(vec![text(&content)?])
}

fn excerpt(text: &str) -> Value {
    let boundary = text.floor_char_boundary(TEXT_LIMIT);
    json!({"text": text.get(..boundary).unwrap_or_default(), "totalBytes": text.len(), "truncated": boundary < text.len()})
}

fn narration(manifest: &Manifest) -> Value {
    let recording = manifest.recording();
    match &recording.narration {
        Narration::Text(value) => {
            json!({"kind": "untimed", "lost": recording.narration_lost, "excerpt": excerpt(value)})
        },
        Narration::Words(words) => {
            let selected: Vec<_> = words
                .iter()
                .take(WORD_LIMIT)
                .scan(0_usize, |bytes, word| {
                    *bytes = bytes.saturating_add(word.text.len());
                    (*bytes <= TEXT_LIMIT).then_some(word)
                })
                .collect();
            let kind = match (words.is_empty(), recording.narration_lost) {
                (false, _) => "timed",
                (true, Some(_)) => "lost",
                (true, None) => "silent",
            };
            json!({"kind": kind, "lost": recording.narration_lost, "words": selected, "totalWords": words.len(), "truncated": selected.len() < words.len()})
        },
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    show: String,
    sheet: Option<usize>,
    keyframe: Option<usize>,
    at: Option<String>,
    mark: Option<usize>,
    phrase: Option<String>,
    #[serde(rename = "match")]
    occurrence: Option<usize>,
}

enum Picture {
    Sheet(usize),
    Keyframe(usize),
    Source(Query),
}

fn image(roots: &Roots, arguments: Value) -> Result<Vec<Value>, String> {
    let request: Image = decode(arguments)?;
    let query = lookup(request.at, request.mark, request.phrase, request.occurrence)?;
    let picture = match (request.sheet, request.keyframe, query) {
        (Some(number), None, None) => Picture::Sheet(number),
        (None, Some(number), None) => Picture::Keyframe(number),
        (None, None, Some(query)) => Picture::Source(query),
        _ => return Err("Choose exactly one sheet, keyframe, time, mark or phrase".to_owned()),
    };
    let directory = roots.resolve(&request.show)?;
    let entry = open(&directory, None)?;
    let manifest = manifest_of(&entry)?;
    let (png, metadata) = match picture {
        Picture::Sheet(number) => {
            let (name, frames) = number
                .checked_sub(1)
                .and_then(|index| manifest.sheets().nth(index))
                .ok_or("That Show has no sheet with that number")?;
            let png = fs::read(directory.join(&name))
                .map_err(|_private| "That saved sheet is unavailable")?;
            (
                png,
                json!({"show": directory, "sheet": number, "frames": frames.iter().map(|frame| frame.index).collect::<Vec<_>>(), "annotations": true}),
            )
        },
        Picture::Keyframe(number) => {
            let frame = number
                .checked_sub(1)
                .and_then(|index| manifest.keyframes().get(index))
                .ok_or("That Show has no keyframe with that number")?;
            let png = fs::read(directory.join(frame.file()))
                .map_err(|_private| "That saved keyframe is unavailable")?;
            let marks: Vec<_> = manifest
                .marks()
                .get(frame.marks.clone())
                .unwrap_or_default()
                .iter()
                .map(|mark| mark.number)
                .collect();
            (
                png,
                json!({"show": directory, "keyframe": number, "atMs": frame.at.as_millis(), "marks": marks, "crop": frame.crop, "annotations": true}),
            )
        },
        Picture::Source(query) => source_image(&directory, manifest, &query)?,
    };
    if !u64::try_from(png.len()).is_ok_and(|length| length <= IMAGE_LIMIT) {
        return Err("The selected image is not a PNG of at most 8 MiB".to_owned());
    }
    let mut reader =
        image::ImageReader::with_format(io::Cursor::new(&png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(PIXEL_EDGE_LIMIT);
    limits.max_image_height = Some(PIXEL_EDGE_LIMIT);
    limits.max_alloc = Some(DECODE_LIMIT);
    reader.limits(limits);
    reader.decode().map_err(
        |_private| "The selected PNG is damaged or exceeds the image dimensions or decoding limit",
    )?;
    Ok(vec![
        text(&metadata)?,
        json!({"type": "image", "data": STANDARD.encode(png), "mimeType": "image/png"}),
    ])
}

fn open(directory: &Path, managed: Option<&Path>) -> Result<recall::Entry, &'static str> {
    recall::Entry::open(directory, managed).map_err(|_private| "Cannot open that Show")
}

fn manifest_of(entry: &recall::Entry) -> Result<&Manifest, &'static str> {
    entry
        .show
        .as_ref()
        .map(|show| &show.manifest)
        .map_err(|_private| "Cannot read that Show's manifest")
}

fn source_image(
    directory: &Path,
    manifest: &Manifest,
    query: &Query,
) -> Result<(Vec<u8>, Value), String> {
    let found = query.resolve(manifest).map_err(|safe| safe.to_string())?;
    let span = found
        .selected
        .ok_or("Several phrase matches exist; use show_read to list matches, then choose match")?;
    let unavailable = |_private| "Retained source frames are unavailable for that Show";
    let mut source = source::Reader::open(directory).map_err(unavailable)?;
    let index = source.inspect().map_err(unavailable)?;
    if index.duration() != manifest.recording().duration {
        return Err("Retained source timing disagrees with the Show manifest".to_owned());
    }
    let frame = source
        .frame(&index, span.start())
        .map_err(|_private| "Cannot read the selected retained source frame")?;
    let png = frame
        .image
        .encode_png()
        .map_err(|_private| "Cannot encode the retained source image")?;
    let metadata = json!({"show": directory, "location": found, "sampledMs": frame.at.as_millis(), "requestedMs": span.start().as_millis(), "samplingStride": index.stride(), "longestGapMs": source::longest_gap(&index), "annotations": false});
    Ok((png, metadata))
}

fn lookup(
    at: Option<String>,
    mark: Option<usize>,
    phrase: Option<String>,
    occurrence: Option<usize>,
) -> Result<Option<Query>, &'static str> {
    const TIME: &str = "Time must be seconds or M:SS.mmm";
    match (at, mark, phrase, occurrence) {
        (None, None, None, None) => Ok(None),
        (Some(at), None, None, None) if at.len() <= TIME_LIMIT => query::time(&at)
            .map(|at| Some(Query::At(at)))
            .map_err(|_private| TIME),
        (Some(_), None, None, None) => Err(TIME),
        (None, Some(0), None, None) | (None, None, Some(_), Some(0)) => {
            Err("Mark and match numbers start at 1")
        },
        (None, Some(mark), None, None) => Ok(Some(Query::Mark(mark))),
        (None, None, Some(phrase), None) => Ok(Some(Query::Phrase(phrase))),
        (None, None, Some(phrase), Some(occurrence)) => {
            Ok(Some(Query::Match { phrase, occurrence }))
        },
        _ => Err("Choose at most one time, mark or phrase; match requires phrase"),
    }
}

fn decode<T: serde::de::DeserializeOwned>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|_private| {
        "Invalid or unknown tool arguments; use the advertised schema".to_owned()
    })
}

fn text(value: &impl Serialize) -> Result<Value, String> {
    let encoded = protocol::encode(value, EVIDENCE_LIMIT)
        .map_err(|_private| "Show evidence exceeds the output limit")?;
    let text = String::from_utf8(encoded).map_err(|_private| "Cannot encode Show evidence")?;
    Ok(json!({"type": "text", "text": text}))
}

#[cfg(test)]
mod tests;
