use super::*;

#[test]
fn malformed_private_settings_report_the_field_without_its_contents() -> anyhow::Result<()> {
    const PRIVATE: &str = "private spoken phrase";
    for key in ["vocabulary", "recognition_terms", "presets"] {
        for value in [
            serde_json::json!(PRIVATE),
            serde_json::json!([PRIVATE]),
            serde_json::json!([{PRIVATE: PRIVATE}]),
        ] {
            let contents = serde_json::to_vec(&serde_json::json!({key: value}))?;
            let result = parse(&contents, Path::new("settings.json"));
            if key == "recognition_terms" && value == serde_json::json!([PRIVATE]) {
                assert!(result.is_ok(), "Valid recognition terms were rejected");
                continue;
            }
            let error = result
                .err()
                .context("Malformed private settings were accepted")?;
            let message = format!("{error:#}");
            assert!(message.contains(key), "The diagnostic omitted its field");
            assert!(
                message.contains("line 1 column"),
                "The diagnostic omitted its location"
            );
            assert!(
                !message.contains(PRIVATE),
                "The diagnostic exposed private content"
            );
            assert!(
                !format!("{error:?}").contains(PRIVATE),
                "The error retained private content"
            );
        }
    }
    Ok(())
}
