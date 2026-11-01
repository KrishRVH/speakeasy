use super::*;

#[test]
fn remote_credential_bearing_and_ambiguous_endpoints_are_refused_without_echoing() {
    for endpoint_text in [
        "http://example.com/",
        "http://192.168.1.1/",
        "http://localhost.evil/",
        "http://PRIVATE_PASSWORD@127.0.0.1/",
        "http://127.0.0.1/?password=PRIVATE_PASSWORD",
        "http://127.0.0.1/#PRIVATE_PASSWORD",
        "http://127.0.0.1/PRIVATE_PASSWORD",
        "file:///PRIVATE_PASSWORD",
        "PRIVATE_PASSWORD",
    ] {
        let error = endpoint(endpoint_text).err().unwrap();
        assert!(!error.contains("PRIVATE_PASSWORD"));
    }
    assert_eq!(
        endpoint("http://localhost:49374").unwrap().as_str(),
        "http://127.0.0.1:49374/"
    );
    assert!(endpoint("http://127.9.8.7:49374/").is_ok());
    assert!(endpoint("http://[::1]:49374/").is_ok());
}

#[test]
fn session_ids_cannot_escape_the_selected_endpoint() {
    for id in [
        "ses_",
        "ses_a/../../prompt",
        "ses_a?query",
        "ses_a#fragment",
        "ses_\nheader",
        "ses_é",
    ] {
        assert!(!valid_id(id, "ses_"));
    }
    assert!(valid_id("ses_explicit-123", "ses_"));
    assert!(!valid_id("ses_explicit-123", "msg_"));
}

#[test]
fn discovery_refuses_oversized_nonregular_and_malformed_registration() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    assert!(registration(directory.path()).is_err());
    let file = directory.path().join("service.json");
    for bytes in [
        b"PRIVATE_MALFORMED_PASSWORD".to_vec(),
        vec![b' '; 65 * 1024],
        br#"{"url":"http://127.0.0.1/","pid":0,"password":"PRIVATE_PASSWORD"}"#.to_vec(),
    ] {
        fs::write(&file, bytes)?;
        assert!(!registration(&file).err().unwrap().contains("PRIVATE"));
    }
    #[cfg(unix)]
    {
        let link = directory.path().join("linked.json");
        std::os::unix::fs::symlink(&file, &link)?;
        assert!(registration(&link).is_err());
    }
    Ok(())
}
