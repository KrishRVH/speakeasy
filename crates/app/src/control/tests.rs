use super::*;
use crate::transcript::style::Case;

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
        parse(&["dictation", "start", "--style", "camel", "--print"])?.command,
        Command::Capture {
            style: Some(Style::Identifier(Case::Camel))
        }
    );
    assert_eq!(
        parse(&["dictation", "stop", "0123456789abcdef-7", "--print"])?.command,
        Command::Print {
            session: "0123456789abcdef-7".into()
        }
    );
    for arguments in [
        vec!["dictation", "stop"],
        vec!["dictation", "stop", "--print"],
        vec!["dictation", "cancel", "0123456789abcdef-7", "--print"],
        vec!["dictation", "start", "--style", "unknown"],
        vec!["status", "paste-last"],
        vec!["status", "--config"],
        vec!["style", "snake", "--demo"],
    ] {
        assert!(parse(&arguments).is_err(), "{arguments:?} parsed");
    }
    Ok(())
}
