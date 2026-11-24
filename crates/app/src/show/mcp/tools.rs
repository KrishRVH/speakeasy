use std::{fs, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use speakeasy_show::{Manifest, Narration, timed_clauses};

use super::{Roots, scope::PAGE};
use crate::show::{
    evidence::{
        Inspection,
        query::{self, Found, Query},
    },
    store::{recall, source},
};

const TEXT_LIMIT: usize = 64 << 10;
const IMAGE_LIMIT: u64 = 8 << 20;

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
    let show = json!({"type": "string", "minLength": 1, "description": "Exact absolute Show path from show_list, or an unambiguous folder name within a configured root. Never latest."});
    let at = json!({"type": "string", "description": "Recording time as seconds or M:SS.mmm."});
    let positive = json!({"type": "integer", "minimum": 1});
    let phrase = json!({"type": "string", "minLength": 1, "description": "Phrase in recorded narration, requiring word timings; not edited summary text."});
    let annotations = json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false});
    json!([
        {"name": Tool::List.name(), "description": format!("List at most {PAGE} saved Shows inside the configured roots, with metadata and a continuation cursor. Roots that cannot be listed are named in unavailableRoots."),
         "inputSchema": {"type": "object", "properties": {"cursor": {"type": "string"}}, "additionalProperties": false}, "annotations": annotations},
        {"name": Tool::Read.name(), "description": format!("Read one exact Show's metadata, contact sheets, keyframes, marks, and edited summary (truncated past {} KiB, and marked so). Optionally include the whole recorded narration as timed rows, and one time, mark, or phrase lookup. Multiple phrase matches stay ambiguous unless match is given.", TEXT_LIMIT >> 10),
         "inputSchema": {"type": "object", "properties": {"show": show, "narration": {"type": "boolean", "default": false}, "at": at, "mark": positive, "phrase": phrase, "match": positive}, "required": ["show"], "additionalProperties": false}, "annotations": annotations},
        {"name": Tool::Image.name(), "description": "Read exactly one PNG, at most 8 MiB. Choose one sheet, keyframe, time, mark, or phrase (with match if ambiguous). Sheet/keyframe numbers start at 1. Time/mark/phrase reads the retained source image at or before the selection, or the first sample if capture began later; sampledMs reports its actual time and requestedMs the selection. Source images have no ink. Missing retained source is an error; no fallback or file writes.",
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
    Ok(vec![text(&roots.list(request.cursor.as_deref()))?])
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
    let summary = recall::summary(&directory).map_err(|error| {
        format!(
            "Can't read the edited summary in {}: {error}",
            directory.display()
        )
    })?;
    let entry = open(&directory, roots.managed())?;
    let manifest = manifest_of(&entry)?;
    let found = query
        .map(|query| query.resolve(manifest))
        .transpose()
        .map_err(|error| error.to_string())?;
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
    let lost = recording.narration_lost;
    match &recording.narration {
        Narration::Text(text) => json!({"kind": "untimed", "lost": lost, "text": text}),
        Narration::Words(words) => {
            let rows: Vec<_> = timed_clauses(words)
                .into_iter()
                .map(|row| json!({"atMs": row.at.as_millis(), "text": row.text}))
                .collect();
            let kind = match (words.is_empty(), lost) {
                (false, _) => "timed",
                (true, Some(_)) => "lost",
                (true, None) => "silent",
            };
            json!({"kind": kind, "lost": lost, "rows": rows})
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
            let png = read_png(&directory.join(&name))?;
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
            let png = read_png(&directory.join(frame.file()))?;
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
        return Err("The selected image is larger than 8 MiB".to_owned());
    }
    Ok(vec![
        text(&metadata)?,
        json!({"type": "image", "data": STANDARD.encode(png), "mimeType": "image/png"}),
    ])
}

fn read_png(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|error| format!("Can't read {}: {error}", path.display()))
}

fn open(directory: &Path, managed: Option<&Path>) -> Result<recall::Entry, String> {
    recall::Entry::open(directory, managed)
        .map_err(|error| format!("Can't open the Show at {}: {error}", directory.display()))
}

fn manifest_of(entry: &recall::Entry) -> Result<&Manifest, String> {
    entry
        .show
        .as_ref()
        .map(|show| &show.manifest)
        .map_err(|error| format!("{error:#}"))
}

fn source_image(
    directory: &Path,
    manifest: &Manifest,
    query: &Query,
) -> Result<(Vec<u8>, Value), String> {
    let found = query.resolve(manifest).map_err(|error| error.to_string())?;
    let span = found
        .selected
        .ok_or("Several phrase matches exist; use show_read to list matches, then choose match")?;
    let unavailable = |error: anyhow::Error| {
        format!("Retained source frames are unavailable for that Show: {error:#}")
    };
    let mut source = source::Reader::open(directory).map_err(unavailable)?;
    let index = source.inspect().map_err(unavailable)?;
    let frame = source
        .frame(&index, span.start())
        .map_err(|error| format!("{error:#}"))?;
    let png = frame
        .image
        .encode_png()
        .map_err(|error| format!("Can't encode the retained source image: {error}"))?;
    let metadata = json!({"show": directory, "location": found, "sampledMs": frame.at.as_millis(), "requestedMs": span.start().as_millis(), "samplingStride": index.stride(), "longestGapMs": source::longest_gap(&index), "annotations": false});
    Ok((png, metadata))
}

fn lookup(
    at: Option<String>,
    mark: Option<usize>,
    phrase: Option<String>,
    occurrence: Option<usize>,
) -> Result<Option<Query>, String> {
    match (at, mark, phrase, occurrence) {
        (None, None, None, None) => Ok(None),
        (Some(at), None, None, None) => query::time(&at)
            .map(|at| Some(Query::At(at)))
            .map_err(|error| format!("Invalid time {at:?}: {error:#}")),
        (None, Some(0), None, None) | (None, None, Some(_), Some(0)) => {
            Err("Mark and match numbers start at 1".to_owned())
        },
        (None, Some(mark), None, None) => Ok(Some(Query::Mark(mark))),
        (None, None, Some(phrase), None) => Ok(Some(Query::Phrase(phrase))),
        (None, None, Some(phrase), Some(occurrence)) => {
            Ok(Some(Query::Match { phrase, occurrence }))
        },
        _ => Err("Choose at most one time, mark or phrase; match requires phrase".to_owned()),
    }
}

fn decode<T: serde::de::DeserializeOwned>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|error| format!("Invalid tool arguments: {error}"))
}

fn text(value: &impl Serialize) -> Result<Value, String> {
    let text = serde_json::to_string(value)
        .map_err(|error| format!("Can't encode Show evidence: {error}"))?;
    Ok(json!({"type": "text", "text": text}))
}

#[cfg(test)]
mod tests;
