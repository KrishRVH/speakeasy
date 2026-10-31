//! Corpus integrity and scoring regressions use synthetic content and fake insertion only.

use super::*;
use crate::runtime::{Ports as _, fixture::Fixture};

fn report(reference: &str, text: &str, latency: Option<f64>, speed: u32) -> Report {
    Report {
        segmenting: false,
        continuous: false,
        speed,
        startup_ms: 0.0,
        shutdown_ms: 0.0,
        stop_to_fake_insert_ms: latency,
        wav_sha256: String::new(),
        reference_sha256: String::new(),
        requests: Vec::new(),
        capture: crate::audio::fixture::Statistics::default(),
        score: Score::new(reference, text),
        total_inference_ms: Some(0.0),
    }
}

#[test]
fn scoring_counts_deletions_and_weights_words_without_accelerated_latency() {
    let reports = [
        report(
            "one two three four five six seven eight nine ten",
            "one two three four five six seven eight nine wrong",
            Some(100.0),
            1,
        ),
        report("missing", "", None, 1),
        report("word", "word", Some(10.0), 1),
        report("word", "word", Some(5.0), 1),
        report("word", "word", Some(0.1), 10),
    ];
    let measured = Metrics::new(reports.iter());
    assert_eq!(measured.word_errors, 2);
    assert_eq!(measured.reference_words, 14);
    assert_eq!(measured.word_error_rate, Some(2.0 / 14.0));
    assert_eq!(measured.clips, 5);
    assert_eq!(measured.inserted_clips, 4);
    assert_eq!(measured.realtime_insertions, 3);
    assert_eq!(measured.stop_to_fake_insert_p50_ms, Some(10.0));
    assert_eq!(measured.stop_to_fake_insert_p95_ms, Some(100.0));
    let accelerated = Metrics::new(reports.iter().filter(|r| r.speed == 10));
    assert_eq!(accelerated.stop_to_fake_insert_p95_ms, None);
}

#[test]
fn scoring_separates_formatting_from_words_and_retains_silence_hallucinations() {
    let score = Score::new("RÉSUMÉ, please.", "résumé please");
    assert_eq!(score.word_errors, 0);
    assert_eq!(score.formatting_word_errors, 2);
    assert_eq!(score.formatting_reference_words, 2);
    let hallucination = report("", "unexpected words", Some(5.0), 1);
    let metrics = Metrics::new(std::iter::once(&hallucination));
    assert_eq!(metrics.word_errors, 2);
    assert_eq!(metrics.word_error_rate, None);
    let encoded = serde_json::to_string(&hallucination).unwrap();
    assert!(!encoded.contains("unexpected words"));
}

#[test]
fn corpus_verifies_both_files_and_rejects_duplicate_cases() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("clip.wav"), b"synthetic audio")?;
    std::fs::write(temp.path().join("reference.txt"), "public words")?;
    let case = serde_json::json!({
        "id": "clip", "tags": ["english", "short"],
        "wav": "clip.wav", "reference": "reference.txt",
        "wav_sha256": sha256(b"synthetic audio"),
        "reference_sha256": sha256(b"public words"),
    });
    let path = temp.path().join("corpus.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({"clips": [case]}))?,
    )?;
    let corpus = Corpus::read(&path)?;
    let case_read = corpus.clips.first().unwrap();
    case_read.load(temp.path())?;
    let reports = [report("public words", "public", Some(7.0), 1)];
    let summary = corpus.summarize(&reports);
    assert_eq!(summary.overall.word_errors, 1);
    assert_eq!(summary.by_tag["short"].word_errors, 1);
    assert_eq!(summary.by_tag["english"].reference_words, 2);
    std::fs::write(temp.path().join("reference.txt"), "changed words")?;
    assert!(
        case_read
            .load(temp.path())
            .err()
            .unwrap()
            .to_string()
            .contains("reference hash mismatch")
    );
    std::fs::write(temp.path().join("reference.txt"), "public words")?;
    std::fs::write(temp.path().join("clip.wav"), b"changed audio")?;
    assert!(
        case_read
            .load(temp.path())
            .err()
            .unwrap()
            .to_string()
            .contains("audio hash mismatch")
    );
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({"clips": [case, case]}))?,
    )?;
    assert!(
        Corpus::read(&path)
            .err()
            .unwrap()
            .to_string()
            .contains("unique")
    );
    Ok(())
}

#[tokio::test]
async fn fake_insertion_accepts_unrelated_speech_but_never_a_cancelled_permit() -> anyhow::Result<()>
{
    let (loaded, _ready) = async_channel::bounded(1);
    let (inserted, submitted) = async_channel::bounded(1);
    let fixture = Fixture {
        wav: Vec::new(),
        loaded,
        inserted,
        replay: None,
        requests: None,
    };
    let (sender, _events) = async_channel::bounded(1);
    let input = speakeasy_platform::InputSender::new(sender);
    let text = "Different public speech.\nNext line.";
    assert!(matches!(
        fixture
            .insert(
                text.into(),
                input.begin().unwrap(),
                speakeasy_platform::Delivery::Paste
            )
            .await?,
        speakeasy_platform::Inserted::Sent
    ));
    assert_eq!(submitted.recv().await?.1, text);
    let permit = input.begin().unwrap();
    input.cancel();
    assert!(matches!(
        fixture
            .insert(text.into(), permit, speakeasy_platform::Delivery::Paste)
            .await?,
        speakeasy_platform::Inserted::Unavailable(_)
    ));
    assert!(submitted.is_empty());
    Ok(())
}
