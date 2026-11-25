use speakeasy_show::{Moment, NarrationLoss};

use super::super::{handoff, narration::Narration};

#[test]
fn a_maximum_saved_summary_still_allows_an_image_with_labeled_context() {
    let summary = "é".repeat(512 * 1024);
    assert!(speakeasy_platform::ClipboardContent::text(summary.clone()).is_ok());
    let narration = Narration::Untimed {
        text: "Recorded phrase.".repeat(4_000),
        incomplete: false,
    };
    let text = handoff::text("Displayed sample.\n".into(), Some(&summary), &narration).unwrap();
    assert!(
        speakeasy_platform::ClipboardContent::moment(text.clone(), 1, 1, vec![0, 0, 0, 255])
            .is_ok()
    );
    assert!(text.contains("Summary excerpt"));
    assert!(text.contains("Recorded phrase."));
}

#[test]
fn evidence_keeps_edited_summary_and_timed_recording_as_distinct_context() {
    let narration = Narration::Timed {
        clauses: vec![
            speakeasy_show::TimedClause {
                at: Moment::from_millis(1_500),
                text: "First original phrase.".into(),
            },
            speakeasy_show::TimedClause {
                at: Moment::from_millis(4_000),
                text: "Second original phrase.".into(),
            },
        ],
        incomplete: true,
    };
    let text = handoff::text(
        "Displayed sample at 0:02.000.\n".into(),
        Some("Edited wording: café 👩‍💻\n  Preserve these spaces."),
        &narration,
    )
    .unwrap();
    assert!(text.starts_with("Displayed sample at 0:02.000.\n"));
    assert!(
        text.contains("## Edited summary\n\nEdited wording: café 👩‍💻\n  Preserve these spaces.")
    );
    assert!(text.contains("## Recorded narration\n\n[0:01.500] First original phrase.\n[0:04.000] Second original phrase."));
    assert!(text.contains("Narration is incomplete."));
}

#[test]
fn evidence_reports_missing_and_untimed_narration_without_inventing_words() {
    for (narration, expected) in [
        (Narration::Silent, "No recorded words."),
        (Narration::Unavailable, "Recorded narration is unavailable."),
        (
            Narration::Lost(NarrationLoss::Incomplete),
            "The recording is incomplete; no words are available.",
        ),
        (
            Narration::Lost(NarrationLoss::AudioStopped),
            "Audio capture stopped; no recorded words are available.",
        ),
        (
            Narration::Lost(NarrationLoss::NotTranscribed),
            "Recorded speech wasn't transcribed.",
        ),
        (
            Narration::Untimed {
                text: "Exact untimed wording.".into(),
                incomplete: false,
            },
            "No word timestamps are available.\nExact untimed wording.",
        ),
    ] {
        let text = handoff::text(String::new(), None, &narration).unwrap();
        assert!(text.contains("The edited summary is unavailable."));
        assert!(text.ends_with(&format!("## Recorded narration\n\n{expected}\n")));
    }
}
