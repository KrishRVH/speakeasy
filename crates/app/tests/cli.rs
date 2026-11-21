#![forbid(unsafe_code)]

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const CLI: &str = env!("CARGO_BIN_EXE_speakeasy-cli");
const APP: &str = env!("CARGO_BIN_EXE_speakeasy");

fn isolated_cli(root: &Path) -> anyhow::Result<PathBuf> {
    let name = Path::new(CLI)
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("The console entry has no file name"))?;
    let isolated = root.join(name);
    if cfg!(unix) {
        let copied = Command::new("cp").arg(CLI).arg(&isolated).status()?;
        anyhow::ensure!(copied.success(), "cp could not isolate the console entry");
    } else {
        fs::copy(CLI, &isolated)?;
    }
    Ok(isolated)
}

#[test]
fn console_entry_waits_and_preserves_output_arguments_and_working_directory() -> anyhow::Result<()>
{
    let version = Command::new(CLI).arg("--version").output()?;
    let original = Command::new(APP).arg("--version").output()?;
    assert!(version.status.success());
    assert_eq!(version.stdout, original.stdout);
    assert_eq!(
        version.stdout,
        format!("speakeasy {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );

    let root = tempfile::tempdir()?;
    let name = "Show ü & %OS%";
    let saved = Command::new(CLI)
        .current_dir(root.path())
        .args(["show", "demo", "--out", name])
        .output()?;
    assert!(saved.status.success());
    assert_eq!(
        saved.stdout,
        fs::read(root.path().join(name).join("summary.md"))?
    );
    let inspected = Command::new(CLI)
        .current_dir(root.path())
        .args(["show", "inspect", name, "--json"])
        .output()?;
    assert!(inspected.status.success());
    serde_json::from_slice::<Value>(&inspected.stdout)?;

    let odd = "missing ü & %OS% \"quoted\" trailing\\";
    let refused = Command::new(CLI)
        .current_dir(root.path())
        .args(["show", "inspect", odd])
        .output()?;
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8(refused.stderr)?.contains(odd));
    Ok(())
}

#[test]
fn an_isolated_console_entry_never_launches_the_app_for_help_or_invalid_options()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let isolated = isolated_cli(root.path())?;
    for args in [vec![], vec!["--help"], vec!["-h"]] {
        let help = Command::new(&isolated).args(args).output()?;
        assert!(help.status.success());
        assert!(String::from_utf8(help.stdout)?.contains("show --help"));
    }
    let rejected = Command::new(&isolated).arg("--demo").output()?;
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8(rejected.stderr)?.contains("Use show"));
    let missing = Command::new(&isolated).args(["show", "list"]).output()?;
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8(missing.stderr)?.contains("Cannot start"));
    Ok(())
}

#[test]
fn mcp_stdio_roundtrips_through_the_console_process() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let name = "Show ü & %OS%";
    let directory = root.path().join(name);
    fs::create_dir_all(directory.join("frames"))?;
    let manifest = json!({
        "version": 3, "duration": 1000, "surface": {"width": 2, "height": 2},
        "strokes": [], "pins": [], "actions": [], "narration": {"words": []},
        "marks": [],
        "keyframes": [{
            "index": 1, "at": 500, "reason": "middle", "event": "public fixture", "marks": [0, 0]
        }]
    });
    fs::write(directory.join("show.json"), serde_json::to_vec(&manifest)?)?;
    let summary = "Public fixture: ü, \"quoted\" and a literal \\ path.\nSecond line.\n";
    fs::write(directory.join("summary.md"), summary)?;
    let png = speakeasy_show::Image::from_bgra(2, 2, [20, 40, 60, 255].repeat(4))?.encode_png()?;
    fs::write(directory.join("contact-sheet.png"), &png)?;
    fs::write(directory.join("frames/frame-01.png"), png)?;
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "public-process-fixture", "version": "1"}
        }}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "show_read", "arguments": {"show": name}
        }}),
    ];
    let mut child = Command::new(CLI)
        .current_dir(root.path())
        .args(["show", "mcp", "--root", "."])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        let mut input = child.stdin.take().unwrap();
        for request in requests {
            serde_json::to_writer(&mut input, &request)?;
            writeln!(input)?;
        }
    }
    let result = child.wait_with_output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = String::from_utf8(result.stdout)?;
    let replies = output
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-11-25");
    let evidence: Value =
        serde_json::from_str(replies[1]["result"]["content"][0]["text"].as_str().unwrap())?;
    assert_eq!(evidence["summary"]["text"], summary);
    Ok(())
}

#[test]
fn console_control_verbs_report_json_when_the_app_is_not_running() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let settings = root.path().join("isolated ü settings.json");
    for arguments in [
        vec!["status"],
        vec!["dictation", "start", "--style", "snake"],
    ] {
        let output = Command::new(CLI)
            .args(arguments)
            .arg("--config")
            .arg(&settings)
            .output()?;
        assert_eq!(output.status.code(), Some(2));
        let reply: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(reply["v"], 1);
        assert_eq!(reply["result"], "unknown");
    }
    Ok(())
}
