use super::*;
use crate::transcript::vocabulary::Correction;

fn config() -> Config {
    Config {
        language: "en-US".into(),
        ..Config::default()
    }
}

fn correction(heard: &str, written: &str) -> Config {
    Config {
        vocabulary: vec![Correction {
            heard: heard.into(),
            written: written.into(),
        }],
        ..config()
    }
}

fn policy(reference: &str, raw: &str, config: &Config) -> Policy {
    Policy::new(
        "current",
        reference,
        raw,
        &Score::new(reference, raw),
        config,
    )
}

#[test]
fn cleanup_scores_help_and_damage_against_the_same_recognition() {
    let helped = policy("send the report", "um send the report", &config());
    assert_eq!(helped.versus_raw.outcome.words, Change::Win);
    assert_eq!(helped.score.word_errors, 0);
    let damaged = policy(
        "send the report",
        "send the report",
        &correction("report", "memo"),
    );
    assert_eq!(damaged.versus_raw.outcome.words, Change::Loss);
    assert_eq!(damaged.versus_raw.removed_reference_words, 1);
    assert_eq!(damaged.versus_raw.introduced_tokens, 1);
    assert_eq!(damaged.versus_raw.digit_tokens_lost, 0);
    let existing = policy(
        "send report",
        "memo send report",
        &correction("report", "memo memo"),
    );
    assert_eq!(existing.versus_raw.introduced_tokens, 0);
    let digits = policy("keep 5 5", "keep 5 5", &correction("5", "five"));
    assert_eq!(digits.versus_raw.digit_tokens_lost, 2);
    let duplicate = policy(
        "keep word",
        "keep word word",
        &correction("word word", "word"),
    );
    assert_eq!(duplicate.versus_raw.removed_reference_words, 0);
    let empty = policy("", "unexpected words", &config());
    assert_eq!(empty.score.word_errors, 2);
    assert_eq!(empty.versus_raw.outcome.words, Change::Tie);
    let formatting = policy("Résumé!", "résumé", &correction("résumé", "Résumé!"));
    assert_eq!(formatting.versus_raw.outcome.words, Change::Tie);
    assert_eq!(formatting.versus_raw.outcome.formatting, Change::Win);
}

#[test]
fn paired_replay_keeps_hints_and_compares_candidate_with_current_without_text() {
    let mut current = config();
    let candidate = correction("report", "memo");
    validate_candidate(&current, &candidate).unwrap();
    let mode = Cleanup::Raw {
        candidate: Some(Box::new(candidate)),
    };
    let evaluation = mode.evaluate("send report", "um send report", &current);
    let Evaluation::Raw { policies, .. } = &evaluation else {
        panic!("Raw replay required");
    };
    assert_eq!(policies[0].versus_raw.outcome.words, Change::Win);
    assert_eq!(policies[1].versus_raw.outcome.words, Change::Tie);
    assert_eq!(policies[1].versus_current.unwrap().words, Change::Loss);
    let encoded = serde_json::to_string(&evaluation).unwrap();
    for phrase in ["send", "report", "memo"] {
        assert!(!encoded.contains(phrase));
    }
    current.recognition_terms.add("private hint").unwrap();
    let (raw, style) = mode.prepare(&current);
    assert_eq!(raw.recognition_terms, current.recognition_terms);
    assert_eq!(style, Style::Verbatim);
    assert!(!raw.remove_fillers);
    assert_eq!(raw.vocabulary.len(), 0);
}

#[test]
fn invalid_modes_and_recognition_changes_are_rejected_before_replay() {
    let current = config();
    assert_eq!(
        Cleanup::read(Some(OsStr::new("secret invalid value")), None, &current)
            .err()
            .unwrap()
            .to_string(),
        "Fixture cleanup must be production or raw"
    );
    assert_eq!(
        Cleanup::read(
            None,
            Some(Path::new("nonexistent-private-candidate")),
            &current
        )
        .err()
        .unwrap()
        .to_string(),
        "A candidate requires raw cleanup replay"
    );
    let mut candidate = current.clone();
    candidate.recognition_terms.add("private hint").unwrap();
    assert_eq!(
        validate_candidate(&current, &candidate)
            .unwrap_err()
            .to_string(),
        "The candidate may change only corrections and filler removal"
    );
    candidate = Config {
        language: "de-DE".into(),
        ..current.clone()
    };
    assert!(validate_candidate(&current, &candidate).is_err());
}

#[test]
fn language_gating_keeps_non_english_fillers_and_summary_has_no_latency() {
    let current = Config {
        language: "de-DE".into(),
        ..config()
    };
    let evaluated = Cleanup::Raw { candidate: None }.evaluate("um", "um", &current);
    let report = report(evaluated);
    let metrics = Metrics::new(std::iter::once(&report));
    assert_eq!(metrics.clips, 1);
    assert_eq!(metrics.raw_inserted_clips, 1);
    assert_eq!(metrics.policies[0].changed_clips, 0);
    assert_eq!(metrics.policies[0].versus_raw.words.ties, 1);
    let encoded = serde_json::to_string(&metrics).unwrap();
    assert!(!encoded.contains("p50"));
    assert!(!encoded.contains("p95"));
}

fn report(evaluation: Evaluation) -> Report {
    Report {
        segmenting: false,
        continuous: false,
        speed: 1,
        startup_ms: 0.0,
        shutdown_ms: 0.0,
        stop_to_fake_insert_ms: Some(12.0),
        wav_sha256: String::new(),
        reference_sha256: String::new(),
        requests: Vec::new(),
        capture: crate::audio::fixture::Statistics::default(),
        evaluation,
        total_inference_ms: Some(0.0),
    }
}

#[test]
fn summaries_keep_per_tag_damage_and_silence_errors() {
    let current = config();
    let cleanup = Cleanup::Raw {
        candidate: Some(Box::new(correction("5", "five"))),
    };
    let reports = [
        report(cleanup.evaluate("keep 5", "keep 5", &current)),
        report(cleanup.evaluate("", "phantom", &current)),
    ];
    let clips: Vec<_> = ["numbers", "silence"].into_iter().map(|tag| serde_json::json!({
        "id": tag, "tags": [tag], "wav": "unused", "reference": "unused", "wav_sha256": "unused", "reference_sha256": "unused",
    })).collect();
    let corpus: corpus::Corpus =
        serde_json::from_value(serde_json::json!({"clips": clips})).unwrap();
    let summary = serde_json::to_value(corpus.summarize(&reports)).unwrap();
    assert_eq!(summary["overall"]["raw"]["word_errors"], 1);
    let policies = summary["overall"]["policies"].as_array().unwrap();
    let candidate = policies
        .iter()
        .find(|policy| policy["name"] == "candidate")
        .unwrap();
    assert_eq!(candidate["word_errors"], 2);
    assert_eq!(candidate["versus_current"]["words"]["losses"], 1);
    assert_eq!(candidate["versus_raw"]["words"]["ties"], 1);
    assert_eq!(candidate["damaged_clips"], 1);
    assert_eq!(candidate["digit_tokens_lost"], 1);
    assert_eq!(candidate["removed_reference_words"], 1);
    assert_eq!(candidate["introduced_tokens"], 1);
    assert_eq!(summary["by_tag"]["silence"]["raw"]["word_errors"], 1);
    assert!(summary["by_tag"]["silence"]["raw"]["word_error_rate"].is_null());
    assert_eq!(summary["by_tag"]["numbers"]["raw"]["reference_words"], 2);
    assert!(!summary.to_string().contains("p95"));
}

#[test]
fn candidate_files_allow_only_text_policy_changes_without_loading_a_worker() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("candidate.json");
    let current = crate::config::valid_config(temp.path())?
        .validated(&path)?
        .into_inner();
    let mut candidate = current.clone();
    candidate.remove_fillers = !current.remove_fillers;
    std::fs::write(&path, serde_json::to_vec(&candidate)?)?;
    assert!(Cleanup::read(Some(OsStr::new("raw")), Some(&path), &current).is_ok());
    candidate.recognition_terms.add("private hint")?;
    std::fs::write(&path, serde_json::to_vec(&candidate)?)?;
    assert_eq!(
        Cleanup::read(Some(OsStr::new("raw")), Some(&path), &current)
            .err()
            .unwrap()
            .to_string(),
        "The candidate may change only corrections and filler removal"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn non_unicode_mode_is_rejected_without_echoing_it() {
    use std::os::unix::ffi::OsStrExt as _;
    let mode = OsStr::from_bytes(b"\xff");
    assert_eq!(
        Cleanup::read(Some(mode), None, &config())
            .err()
            .unwrap()
            .to_string(),
        "Fixture cleanup must be production or raw"
    );
}
