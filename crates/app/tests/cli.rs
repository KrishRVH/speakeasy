//! Process-boundary checks for the console entry. Only synthetic Shows and temporary paths are
//! used; no app, model, microphone, global input, or clipboard is opened.

#![forbid(unsafe_code)]

use std::{fs, path::Path, process::Command};

const CLI: &str = env!("CARGO_BIN_EXE_speakeasy-cli");
const APP: &str = env!("CARGO_BIN_EXE_speakeasy");

#[test]
fn console_entry_waits_and_preserves_output_arguments_and_working_directory() {
    let version = Command::new(CLI).arg("--version").output().unwrap();
    let original = Command::new(APP).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(version.stdout, original.stdout);
    assert_eq!(
        version.stdout,
        format!("speakeasy {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );

    let root = tempfile::tempdir().unwrap();
    let name = "Show ü & %OS%";
    let saved = Command::new(CLI)
        .current_dir(root.path())
        .args(["show", "demo", "--out", name])
        .output()
        .unwrap();
    assert!(saved.status.success());
    assert_eq!(
        saved.stdout,
        fs::read(root.path().join(name).join("summary.md")).unwrap()
    );
    let inspected = Command::new(CLI)
        .current_dir(root.path())
        .args(["show", "inspect", name, "--json"])
        .output()
        .unwrap();
    assert!(inspected.status.success());
    assert!(serde_json::from_slice::<serde_json::Value>(&inspected.stdout).is_ok());

    // Direct process invocation avoids PowerShell 5.1's own embedded-quote argument rewriting.
    let odd = "missing ü & %OS% \"quoted\" trailing\\";
    let refused = Command::new(CLI)
        .current_dir(root.path())
        .args(["show", "inspect", odd])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8(refused.stderr).unwrap().contains(odd));
}

#[test]
fn an_isolated_console_entry_never_launches_the_app_for_help_or_invalid_options() {
    let root = tempfile::tempdir().unwrap();
    let isolated = root.path().join(Path::new(CLI).file_name().unwrap());
    fs::copy(CLI, &isolated).unwrap();
    for args in [vec![], vec!["--help"], vec!["-h"]] {
        let help = Command::new(&isolated).args(args).output().unwrap();
        assert!(help.status.success());
        assert!(
            String::from_utf8(help.stdout)
                .unwrap()
                .contains("show --help")
        );
    }
    let rejected = Command::new(&isolated).arg("--demo").output().unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    assert!(
        String::from_utf8(rejected.stderr)
            .unwrap()
            .contains("Use show")
    );
    let missing = Command::new(&isolated)
        .args(["show", "list"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("extract the whole package")
    );
}
