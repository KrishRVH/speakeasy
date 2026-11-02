use anyhow::Context as _;
use speakeasy_platform::ClipboardContent;
use speakeasy_show::NarrationLoss;

use super::{
    narration::{Narration, excerpt},
    navigation::Stamp,
};

const TOO_MUCH: &str = "Too much evidence context to copy. Export the Show instead.";
const EXCERPT: &str = "\n\nSummary excerpt: the complete edited text is in summary.md.";

pub(super) fn text(
    context: String,
    summary: Option<&str>,
    narration: &Narration,
) -> anyhow::Result<String> {
    let summary = summary.unwrap_or("The edited summary is unavailable.");
    let recorded = match narration {
        Narration::Timed { clauses, .. } => clauses
            .iter()
            .map(|clause| format!("[{}] {}", Stamp(clause.at), clause.text))
            .collect::<Vec<_>>()
            .join("\n"),
        Narration::Untimed { text, .. } => format!("No word timestamps are available.\n{text}"),
        Narration::Silent => "No recorded words.".into(),
        Narration::Lost(NarrationLoss::Incomplete) => {
            "The recording is incomplete; no words are available.".into()
        },
        Narration::Lost(NarrationLoss::AudioStopped) => {
            "Audio capture stopped; no recorded words are available.".into()
        },
        Narration::Lost(NarrationLoss::NotTranscribed) => {
            "Recorded speech was not transcribed.".into()
        },
        Narration::Unavailable => "Recorded narration is unavailable.".into(),
    };
    let incomplete = if narration.incomplete() {
        "\nNarration is incomplete."
    } else {
        ""
    };
    let truncated = if narration.truncated() {
        "\nThis excerpt is truncated. The remaining recorded words are in show.json."
    } else {
        ""
    };
    let mut text = format!("{context}\n## Edited summary\n\n");
    let suffix = format!("\n\n## Recorded narration\n\n{recorded}{incomplete}{truncated}\n");
    let available = ClipboardContent::MAX_TEXT_BYTES
        .checked_sub(text.len())
        .and_then(|bytes| bytes.checked_sub(suffix.len()))
        .context(TOO_MUCH)?;
    if summary.len() > available {
        let end = available.checked_sub(EXCERPT.len()).context(TOO_MUCH)?;
        text.push_str(excerpt(summary, end));
        text.push_str(EXCERPT);
    } else {
        text.push_str(summary);
    }
    text.push_str(&suffix);
    Ok(text)
}
