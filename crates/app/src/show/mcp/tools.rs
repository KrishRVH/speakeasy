use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use speakeasy_show::{Manifest, Narration};

use super::{DISCLOSURE, Roots, protocol};
use crate::show::{
    evidence::{
        Inspection, Overview,
        query::{self, Found, Query},
    },
    store::{recall, source},
};

const TEXT_LIMIT: usize = 64 << 10;
const WORD_LIMIT: usize = 512;
const IMAGE_LIMIT: u64 = 8 << 20;

pub(super) fn known(name: &str) -> bool {
    matches!(name, "show_list" | "show_read" | "show_image")
}

pub(super) fn definitions() -> Value {
    let show = json!({"type": "string", "minLength": 1, "maxLength": 4096, "description": "Exact absolute Show path from show_list, or an unambiguous folder name within a configured root. Never latest."});
    let at = json!({"type": "string", "maxLength": 32, "description": "Recording time as seconds or M:SS.mmm."});
    let positive = json!({"type": "integer", "minimum": 1});
    let phrase = json!({"type": "string", "minLength": 1, "maxLength": 2048, "description": "Phrase in recorded narration, requiring word timings; not edited summary text."});
    let annotations = json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false});
    json!([
        {"name": "show_list", "description": format!("List at most 100 saved Shows inside launch-time roots, with metadata only and a continuation cursor. {DISCLOSURE}"),
         "inputSchema": {"type": "object", "properties": {"cursor": {"type": "string", "maxLength": 4096}}, "additionalProperties": false}, "annotations": annotations},
        {"name": "show_read", "description": format!("Read one exact Show's edited summary and metadata. Optionally include bounded recorded narration and one time, mark, or phrase lookup. Multiple phrase matches remain ambiguous unless match is given. Text truncation and missing or untimed narration are disclosed. {DISCLOSURE}"),
         "inputSchema": {"type": "object", "properties": {"show": show, "narration": {"type": "boolean", "default": false}, "at": at, "mark": positive, "phrase": phrase, "match": positive}, "required": ["show"], "additionalProperties": false}, "annotations": annotations},
        {"name": "show_image", "description": format!("Read exactly one PNG, at most 8 MiB. Choose one sheet, keyframe, time, mark, or phrase (with match if ambiguous). Sheet/keyframe numbers start at 1. Time/mark/phrase reads the retained source image at or before the selection, or the first sample if capture began later, with actual sample time. Source images have no ink. Missing retained source is an error; no fallback or file writes. {DISCLOSURE}"),
         "inputSchema": {"type": "object", "properties": {"show": show, "sheet": positive, "keyframe": positive, "at": at, "mark": positive, "phrase": phrase, "match": positive}, "required": ["show"], "oneOf": [{"required": ["sheet"]}, {"required": ["keyframe"]}, {"required": ["at"]}, {"required": ["mark"]}, {"required": ["phrase"]}], "additionalProperties": false}, "annotations": annotations}
    ])
}

pub(super) fn call(roots: &Roots, name: &str, arguments: Value) -> Value {
    let result = match name {
        "show_list" => list(roots, arguments),
        "show_read" => read(roots, arguments),
        "show_image" => image(roots, arguments),
        _ => Err("Unknown Show tool".to_owned()),
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    shows: Vec<Overview>,
    truncated: bool,
    next_cursor: Option<String>,
}

fn list(roots: &Roots, arguments: Value) -> Result<Vec<Value>, String> {
    let request: List = decode(arguments)?;
    if request
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.len() > 4096)
    {
        return Err("Invalid Show list cursor".to_owned());
    }
    let (shows, cursor) = roots.list(request.cursor.as_deref())?;
    Ok(vec![text(&Listed {
        shows,
        truncated: cursor.is_some(),
        next_cursor: cursor,
    })?])
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
    let entry = recall::Entry::open(&directory).map_err(|_private| "Cannot read that Show")?;
    let show = entry
        .show
        .as_ref()
        .map_err(|_private| "That Show's manifest is damaged or unsupported")?;
    let summary =
        recall::summary(&directory).map_err(|_private| "Cannot read that Show's edited summary")?;
    let found = query
        .as_ref()
        .map(|query| query.resolve(&show.manifest))
        .transpose()
        .map_err(|safe| safe.to_string())?;
    let narration = request.narration.then(|| narration(&show.manifest));
    let content = ReadEvidence {
        show: Inspection::of(&entry, directory.parent()),
        summary: excerpt(&summary),
        narration,
        location: found,
    };
    Ok(vec![text(&content)?])
}

fn excerpt(text: &str) -> Value {
    let boundary = text.floor_char_boundary(TEXT_LIMIT.min(text.len()));
    json!({"text": text.get(..boundary).unwrap_or_default(), "totalBytes": text.len(), "truncated": boundary < text.len()})
}

fn narration(manifest: &Manifest) -> Value {
    let recording = manifest.recording();
    match &recording.narration {
        Narration::Text(value) => {
            json!({"kind": "untimed", "lost": recording.narration_lost, "excerpt": excerpt(value)})
        },
        Narration::Words(words) => {
            let mut bytes = 0_usize;
            let selected: Vec<_> = words
                .iter()
                .take(WORD_LIMIT)
                .take_while(|word| {
                    bytes = bytes.saturating_add(word.text.len());
                    bytes <= TEXT_LIMIT
                })
                .collect();
            json!({"kind": if words.is_empty() { if recording.narration_lost.is_some() { "lost" } else { "silent" } } else { "timed" }, "lost": recording.narration_lost, "words": selected, "totalWords": words.len(), "truncated": selected.len() < words.len()})
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

fn image(roots: &Roots, arguments: Value) -> Result<Vec<Value>, String> {
    let request: Image = decode(arguments)?;
    let query = lookup(request.at, request.mark, request.phrase, request.occurrence)?;
    if usize::from(request.sheet.is_some())
        .saturating_add(usize::from(request.keyframe.is_some()))
        .saturating_add(usize::from(query.is_some()))
        != 1
    {
        return Err("Choose exactly one sheet, keyframe, time, mark or phrase".to_owned());
    }
    let directory = roots.resolve(&request.show)?;
    let entry = recall::Entry::open(&directory).map_err(|_private| "Cannot read that Show")?;
    let show = entry
        .show
        .as_ref()
        .map_err(|_private| "That Show's manifest is damaged or unsupported")?;
    let manifest = &show.manifest;
    let (png, metadata) = if let Some(number) = request.sheet {
        let (name, frames) = number
            .checked_sub(1)
            .and_then(|index| manifest.sheets().nth(index))
            .ok_or("That Show has no sheet with that number")?;
        let png = recall::read_file(&directory, &name, IMAGE_LIMIT)
            .map_err(|_private| "That saved sheet is unavailable or exceeds 8 MiB")?;
        (
            png,
            json!({"show": directory, "sheet": number, "frames": frames.iter().map(|frame| frame.index).collect::<Vec<_>>(), "annotations": true}),
        )
    } else if let Some(number) = request.keyframe {
        let frame = number
            .checked_sub(1)
            .and_then(|index| manifest.keyframes().get(index))
            .ok_or("That Show has no keyframe with that number")?;
        let png = recall::read_file(&directory, &frame.file(), IMAGE_LIMIT)
            .map_err(|_private| "That saved keyframe is unavailable or exceeds 8 MiB")?;
        let marks: Vec<_> = manifest
            .marks()
            .get(frame.marks.clone())
            .unwrap_or_default()
            .iter()
            .map(|mark| mark.number)
            .collect();
        (
            png,
            json!({"show": directory, "keyframe": number, "atMs": frame.at.as_millis(), "marks": marks, "annotations": true}),
        )
    } else {
        let query = query.ok_or("A source image needs a time, mark or phrase")?;
        source_image(&directory, manifest, &query)?
    };
    if !u64::try_from(png.len()).is_ok_and(|length| length <= IMAGE_LIMIT) {
        return Err("The selected image is not a PNG of at most 8 MiB".to_owned());
    }
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 << 20);
    reader.limits(limits);
    reader.decode().map_err(
        |_private| "The selected PNG is damaged or exceeds the image dimensions or decoding limit",
    )?;
    Ok(vec![
        text(&metadata)?,
        json!({"type": "image", "data": STANDARD.encode(png), "mimeType": "image/png"}),
    ])
}

fn source_image(
    directory: &std::path::Path,
    manifest: &Manifest,
    query: &Query,
) -> Result<(Vec<u8>, Value), String> {
    let found: Found = query.resolve(manifest).map_err(|safe| safe.to_string())?;
    let span = found
        .selected
        .ok_or("Several phrase matches exist; use show_read to list matches, then choose match")?;
    let index = source::inspect(directory)
        .map_err(|_private| "Retained source frames are unavailable for that Show")?;
    if index.duration() != manifest.recording().duration {
        return Err("Retained source timing disagrees with the Show manifest".to_owned());
    }
    let frame = source::frame(directory, &index, span.start())
        .map_err(|_private| "Cannot read the selected retained source frame")?;
    let png = frame
        .image
        .encode_png()
        .map_err(|_private| "Cannot encode the retained source image")?;
    let metadata = json!({"show": directory, "location": found, "sampleAtMs": frame.at.as_millis(), "requestedAtMs": span.start().as_millis(), "sampleAfterRequest": frame.at > span.start(), "samplingStride": index.stride(), "longestGapMs": source::longest_gap(&index), "annotations": false});
    Ok((png, metadata))
}

fn lookup(
    at: Option<String>,
    mark: Option<usize>,
    phrase: Option<String>,
    occurrence: Option<usize>,
) -> Result<Option<Query>, String> {
    let selectors = usize::from(at.is_some())
        .saturating_add(usize::from(mark.is_some()))
        .saturating_add(usize::from(phrase.is_some()));
    if selectors > 1 || occurrence.is_some() && phrase.is_none() {
        return Err("Choose at most one time, mark or phrase; match requires phrase".to_owned());
    }
    if mark == Some(0) || occurrence == Some(0) {
        return Err("Mark and match numbers start at 1".to_owned());
    }
    if let Some(at) = at {
        if at.len() > 32 {
            return Err("Time must be seconds or M:SS.mmm".to_owned());
        }
        return query::time(&at)
            .map(|at| Some(Query::At(at)))
            .map_err(|_private| "Time must be seconds or M:SS.mmm".to_owned());
    }
    if let Some(mark) = mark {
        return Ok(Some(Query::Mark(mark)));
    }
    Ok(phrase.map(|phrase| match occurrence {
        Some(occurrence) => Query::Match { phrase, occurrence },
        None => Query::Phrase(phrase),
    }))
}

fn decode<T: serde::de::DeserializeOwned>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|_private| {
        "Invalid or unknown tool arguments; use the advertised schema".to_owned()
    })
}

fn text(value: &impl Serialize) -> Result<Value, String> {
    let encoded = protocol::encode_with_limit(value, 1 << 20)
        .map_err(|_private| "Show evidence exceeds the output limit")?;
    let text = String::from_utf8(encoded).map_err(|_private| "Cannot encode Show evidence")?;
    Ok(json!({"type": "text", "text": text}))
}

#[cfg(test)]
mod tests;
