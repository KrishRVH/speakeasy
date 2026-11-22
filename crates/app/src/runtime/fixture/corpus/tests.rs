use super::*;

fn report(reference: &str, text: &str, latency: Option<f64>, speed: u32) -> Report {
    Report::scored(
        paired::Evaluation::Production(Score::new(reference, text)),
        latency,
        speed,
    )
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
    assert_eq!(measured.quality.word_errors, 2);
    assert_eq!(measured.quality.reference_words, 14);
    assert_eq!(measured.quality.word_error_rate, Some(2.0 / 14.0));
    assert_eq!(measured.clips, 5);
    assert_eq!(measured.inserted_clips, 4);
    assert_eq!(measured.realtime_insertions, 3);
    assert_eq!(measured.stop_to_fake_insert_p50_ms, Some(10.0));
    assert_eq!(measured.stop_to_fake_insert_p95_ms, Some(100.0));
    let accelerated = Metrics::new(reports.iter().filter(|report| report.speed == 10));
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
    assert_eq!(metrics.quality.word_errors, 2);
    assert_eq!(metrics.quality.word_error_rate, None);
    let encoded = serde_json::to_string(&hallucination).unwrap();
    assert!(!encoded.contains("unexpected words"));
}

#[test]
fn production_summaries_count_a_report_under_every_tag_of_its_case() {
    let case = Case {
        id: "clip".into(),
        tags: BTreeSet::from(["english".into(), "short".into()]),
        wav: PathBuf::from("clip.wav"),
        reference: PathBuf::from("reference.txt"),
    };
    let results = [(&case, report("public words", "public", Some(7.0), 1))];
    let Summary::Production { overall, by_tag } = Summary::new(&results) else {
        panic!("Production summary required");
    };
    assert_eq!(overall.quality.word_errors, 1);
    assert_eq!(by_tag["short"].quality.word_errors, 1);
    assert_eq!(by_tag["english"].quality.reference_words, 2);
}
