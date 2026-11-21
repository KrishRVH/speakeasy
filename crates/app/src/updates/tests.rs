use super::*;

#[test]
fn only_a_newer_numeric_release_offers_its_link() {
    let body = br#"{"tag_name":"v1.10.0"}"#;
    let version = newer(body, "1.9.99").unwrap().unwrap();
    assert_eq!(
        version.release_url(),
        "https://github.com/KrishRVH/speakeasy/releases/tag/v1.10.0"
    );
    assert!(newer(body, "1.10.0").unwrap().is_none());
    assert!(newer(body, "2.0.0").unwrap().is_none());
    for tag in ["1.2.3", "v1.2", "v1.2.3.4", "v1.2.3-beta", "nightly"] {
        let body = serde_json::to_vec(&serde_json::json!({"tag_name":tag})).unwrap();
        assert!(newer(&body, "0.7.0").is_err(), "{tag}");
    }
}
