use super::*;
use crate::transcript::style::Case;

fn status() -> Status {
    Status {
        version: "0.7.0".into(),
        service: "running".into(),
        phase: "recording".into(),
        model: "ready".into(),
        desktop_ready: true,
        session: Some("0123456789abcdef-7".into()),
        style: Style::Prose,
        preset: Some("Fixture preset".into()),
        last_dictation: false,
    }
}

#[test]
fn wire_commands_preserve_atomic_style_and_session_identity() {
    let cases = [
        (r#"{"v":1,"op":"status"}"#, Command::Status),
        (r#"{"v":1,"op":"start"}"#, Command::Start { style: None }),
        (
            r#"{"v":1,"op":"start","style":"snake"}"#,
            Command::Start {
                style: Some(Style::Identifier(Case::Snake)),
            },
        ),
        (
            r#"{"v":1,"op":"stop","session":"0123456789abcdef-7"}"#,
            Command::Stop {
                session: "0123456789abcdef-7".into(),
            },
        ),
        (
            r#"{"v":1,"op":"cancel","session":"0123456789abcdef-7"}"#,
            Command::Cancel {
                session: "0123456789abcdef-7".into(),
            },
        ),
        (r#"{"v":1,"op":"paste_last"}"#, Command::PasteLast),
        (
            r#"{"v":1,"op":"style","name":"verbatim"}"#,
            Command::Style {
                name: Style::Verbatim,
            },
        ),
        (
            r#"{"v":1,"op":"preset","name":"Fixture preset"}"#,
            Command::Preset {
                name: "Fixture preset".into(),
            },
        ),
    ];
    for (json, command) in cases {
        assert_eq!(Command::decode(json.as_bytes()).unwrap(), command);
        assert_eq!(
            Command::decode(&command.encode().unwrap()).unwrap(),
            command
        );
    }
}

#[test]
fn malformed_and_cross_operation_arguments_are_refused_without_echoing_content() {
    for json in [
        r#"{"v":2,"op":"status"}"#,
        r#"{"v":1,"op":"start","style":"private fixture"}"#,
        r#"{"v":1,"op":"status","name":"private fixture"}"#,
        r#"{"v":1,"op":"stop"}"#,
        r#"{"v":1,"op":"cancel","session":""}"#,
        r#"{"v":1,"op":"paste_last","history":true}"#,
        r#"{"v":1,"op":"preset","name":" padded "}"#,
        r#"{"v":1,"op":"private fixture"}"#,
        r#"{"v":1,"v":1,"op":"status"}"#,
        "[]",
    ] {
        let error = Command::decode(json.as_bytes()).unwrap_err();
        assert!(!error.contains("private fixture"));
    }
    let oversized = Command::Preset {
        name: "x".repeat(MAX_BYTES),
    };
    assert!(oversized.encode().is_err());
}

#[test]
fn disconnected_and_expired_calls_cannot_authorize_work() {
    let (call, receipt) = Call::new(Command::Start { style: None });
    assert!(call.is_live());
    drop(receipt);
    assert!(!call.is_live());
    let (mut call, _receipt) = Call::new(Command::Start { style: None });
    call.deadline = Instant::now();
    assert!(!call.is_live());
}

#[test]
fn forwarded_calls_lose_authority_when_the_original_caller_disconnects() {
    let (call, caller) = Call::new(Command::Preset {
        name: "Fixture preset".into(),
    });
    let (forwarded, confirmation) = call.forwarded();
    assert!(forwarded.is_live());
    drop(caller);
    assert!(!forwarded.reply.is_closed());
    assert!(confirmation.try_recv().is_err());
    assert!(!forwarded.is_live());
}

#[test]
fn forwarding_an_expired_call_cannot_renew_its_authority() {
    let (mut call, _caller) = Call::new(Command::Preset {
        name: "Fixture preset".into(),
    });
    call.deadline = Instant::now();
    let (forwarded, _confirmation) = call.forwarded();
    assert!(!forwarded.is_live());
}

#[test]
fn replies_are_one_content_free_json_line_with_distinct_exit_dispositions() -> anyhow::Result<()> {
    for (reply, exit) in [
        (Reply::ok(status()), 0),
        (Reply::refused("busy"), 1),
        (Reply::unknown("unconfirmed"), 2),
    ] {
        let mut output = Vec::new();
        reply.write(&mut output)?;
        assert_eq!(reply.exit_code(), exit);
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
        let parsed: serde_json::Value = serde_json::from_slice(&output)?;
        assert_eq!(parsed["v"], 1);
        if let Some(status) = parsed.get("status") {
            let keys: Vec<_> = status
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                [
                    "version",
                    "service",
                    "phase",
                    "model",
                    "desktop_ready",
                    "session",
                    "style",
                    "preset",
                    "last_dictation"
                ]
            );
        }
        let debug = format!("{reply:?}");
        assert!(!debug.contains("0123456789abcdef"));
        assert!(!debug.contains("Fixture preset"));
    }
    let command = Command::Stop {
        session: "private fixture".into(),
    };
    assert!(!format!("{command:?}").contains("private fixture"));
    assert!(Reply::decode(br#"{"v":2,"result":"ok"}"#).is_err());
    Ok(())
}

#[test]
fn cli_parsing_keeps_config_separate_and_rejects_ambiguous_calls() -> anyhow::Result<()> {
    let parse = |arguments: &[&str]| Invocation::parse(arguments.iter().map(OsString::from));
    let call = parse(&[
        "--config",
        "fixture.json",
        "dictation",
        "start",
        "--style",
        "camel",
    ])?;
    assert_eq!(call.settings, Some(PathBuf::from("fixture.json")));
    assert_eq!(
        call.command,
        Command::Start {
            style: Some(Style::Identifier(Case::Camel))
        }
    );
    assert_eq!(
        parse(&["preset", "Fixture preset"])?.command,
        Command::Preset {
            name: "Fixture preset".into()
        }
    );
    for arguments in [
        vec!["dictation", "stop"],
        vec!["dictation", "cancel", ""],
        vec!["dictation", "start", "--style", "unknown"],
        vec!["status", "paste-last"],
        vec!["status", "--config"],
        vec!["status", "--config", "a", "--config", "b"],
        vec!["style", "snake", "--demo"],
    ] {
        assert!(parse(&arguments).is_err());
    }
    Ok(())
}
