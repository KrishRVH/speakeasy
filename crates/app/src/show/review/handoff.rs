//! Displayed evidence, edited summary, and original narration in one bounded copy.

use anyhow::Context as _;
use speakeasy_platform::ClipboardContent;
use speakeasy_show::NarrationLoss;

use super::{narration::Narration, navigation::Stamp};

pub(super) fn text(
    context: String,
    summary: Option<&str>,
    narration: &Narration,
) -> anyhow::Result<String> {
    let summary = summary.unwrap_or("The edited summary is unavailable.");
    let (recorded, incomplete, truncated) = match narration {
        Narration::Timed {
            clauses,
            incomplete,
            truncated,
        } => (
            clauses
                .iter()
                .map(|clause| format!("[{}] {}", Stamp(clause.at), clause.text))
                .collect::<Vec<_>>()
                .join("\n"),
            *incomplete,
            *truncated,
        ),
        Narration::Untimed {
            text,
            incomplete,
            truncated,
        } => (
            format!("No word timestamps are available.\n{text}"),
            *incomplete,
            *truncated,
        ),
        Narration::Silent => ("No recorded words.".into(), false, false),
        Narration::Lost(loss) => (
            match loss {
                NarrationLoss::Incomplete => "The recording is incomplete; no words are available.",
                NarrationLoss::AudioStopped => {
                    "Audio capture stopped; no recorded words are available."
                },
                NarrationLoss::NotTranscribed => "Recorded speech was not transcribed.",
            }
            .into(),
            false,
            false,
        ),
        Narration::Unavailable => ("Recorded narration is unavailable.".into(), false, false),
    };
    let incomplete = if incomplete {
        "\nNarration is incomplete."
    } else {
        ""
    };
    let truncated = if truncated {
        "\nThis excerpt is truncated. The remaining recorded words are in show.json."
    } else {
        ""
    };
    let mut text = format!("{context}\n## Edited summary\n\n");
    let suffix = format!("\n\n## Recorded narration\n\n{recorded}{incomplete}{truncated}\n");
    let available = ClipboardContent::MAX_TEXT_BYTES
        .checked_sub(text.len())
        .and_then(|bytes| bytes.checked_sub(suffix.len()))
        .context("Too much evidence context to copy. Export the Show instead.")?;
    if summary.len() > available {
        let label = "\n\nSummary excerpt: the complete edited text is in summary.md.";
        let end = available
            .checked_sub(label.len())
            .context("Too much evidence context to copy. Export the Show instead.")?;
        let excerpt = summary
            .get(..summary.floor_char_boundary(end))
            .context("Cannot prepare the summary excerpt")?;
        text.push_str(excerpt);
        text.push_str(label);
    } else {
        text.push_str(summary);
    }
    text.push_str(&suffix);
    Ok(text)
}
