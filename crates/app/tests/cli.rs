#![forbid(unsafe_code)]

use std::{
    fs,
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const CLI: &str = env!("CARGO_BIN_EXE_speakeasy-cli");
const APP: &str = env!("CARGO_BIN_EXE_speakeasy");

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
    let isolated = root.path().join(Path::new(CLI).file_name().unwrap());
    fs::copy(CLI, &isolated)?;
    for args in [vec![], vec!["--help"], vec!["-h"]] {
        let help = Command::new(&isolated).args(args).output()?;
        assert!(help.status.success());
        assert!(String::from_utf8(help.stdout)?.contains("show --help"));
    }
    let rejected = Command::new(&isolated).arg("--demo").output()?;
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8(rejected.stderr)?.contains("Use show"));
    let missing = Command::new(&isolated).args(["show", "list"]).output()?;
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8(missing.stderr)?.contains("extract the whole package"));
    Ok(())
}

#[test]
fn mcp_stdio_roundtrips_both_protocols_through_the_console_process() -> anyhow::Result<()> {
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
    for modern in [false, true] {
        let first = if modern {
            json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}})
        } else {
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "public-process-fixture", "version": "1"}
            }})
        };
        let mut requests = vec![first];
        if !modern {
            requests.push(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        }
        requests.extend([
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                "name": "show_read", "arguments": {"show": name}
            }}),
        ]);
        if modern {
            for request in &mut requests {
                request["params"]["_meta"] = json!({
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                });
            }
        }
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
        assert_eq!(result.stderr, b"");
        let output = String::from_utf8(result.stdout)?;
        let replies = output
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(replies.len(), 3);
        for (reply, id) in replies.iter().zip(1..=3) {
            assert_eq!(reply["jsonrpc"], "2.0");
            assert_eq!(reply["id"], id);
            assert!(reply.get("error").is_none());
        }
        if modern {
            assert_eq!(replies[0]["result"]["resultType"], "complete");
        } else {
            assert_eq!(replies[0]["result"]["protocolVersion"], "2025-11-25");
        }
        assert!(
            replies[1]["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "show_read")
        );
        assert_eq!(replies[2]["result"]["isError"], false);
        let evidence: Value =
            serde_json::from_str(replies[2]["result"]["content"][0]["text"].as_str().unwrap())?;
        assert_eq!(evidence["summary"]["text"], summary);
        assert_eq!(fs::read_to_string(directory.join("summary.md"))?, summary);
    }
    Ok(())
}

#[test]
fn console_control_verbs_reach_the_app_and_preserve_json_exit_status_without_a_desktop()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let settings = root.path().join("isolated ü settings.json");
    for arguments in [
        vec!["status"],
        vec!["dictation", "start", "--style", "snake"],
        vec!["dictation", "stop", "0000000000000001-1"],
        vec!["dictation", "cancel", "0000000000000001-1"],
        vec!["paste-last"],
        vec!["style", "verbatim"],
        vec!["preset", "Public preset ü &"],
    ] {
        let output = Command::new(CLI)
            .args(arguments)
            .arg("--config")
            .arg(&settings)
            .output()?;
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(output.stderr, b"");
        let text = String::from_utf8(output.stdout)?;
        assert_eq!(text.lines().count(), 1);
        let reply: Value = serde_json::from_str(&text)?;
        assert_eq!(reply["v"], 1);
        assert_eq!(reply["result"], "unknown");
        assert!(reply.get("status").is_none());
    }
    assert!(fs::read_dir(root.path())?.next().is_none());
    Ok(())
}

#[test]
fn a_closed_control_stdout_never_claims_an_acknowledged_action_was_refused() -> anyhow::Result<()> {
    use std::{
        io::Read as _,
        net::{Ipv4Addr, TcpListener},
        thread,
        time::{Duration, Instant},
    };

    for (result, expected) in [("ok", 2), ("refused", 1), ("unknown", 2)] {
        let root = tempfile::tempdir()?;
        let settings = root.path().join("settings.json");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        fs::write(
            root.path().join("instance.port"),
            format!("{} {:032x}", listener.local_addr()?.port(), 1_u128),
        )?;
        let (closed, stdout_closed) = std::sync::mpsc::channel();
        let server = thread::spawn(move || -> anyhow::Result<()> {
            let deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        anyhow::ensure!(
                            Instant::now() < deadline,
                            "Fixture caller did not connect"
                        );
                        thread::sleep(Duration::from_millis(10));
                    },
                    Err(error) => return Err(error.into()),
                }
            };
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            let mut nonce = [0; 16];
            stream.read_exact(&mut nonce)?;
            assert_eq!(u128::from_le_bytes(nonce), 1);
            stream.write_all(&[0x06])?;
            let mut header = [0; 3];
            stream.read_exact(&mut header)?;
            assert_eq!(header[0], 3);
            let mut request = vec![0; usize::from(u16::from_be_bytes([header[1], header[2]]))];
            stream.read_exact(&mut request)?;
            let request: Value = serde_json::from_slice(&request)?;
            assert_eq!(request["op"], "start");
            stdout_closed.recv_timeout(Duration::from_secs(5))?;
            let reply = serde_json::to_vec(&json!({"v": 1, "result": result}))?;
            stream.write_all(&u16::try_from(reply.len())?.to_be_bytes())?;
            stream.write_all(&reply)?;
            Ok(())
        });
        let mut child = Command::new(APP)
            .args(["dictation", "start", "--config"])
            .arg(&settings)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        drop(child.stdout.take());
        closed.send(())?;
        let output = child.wait_with_output()?;
        server.join().unwrap()?;
        assert_eq!(output.status.code(), Some(expected), "{result}");
        assert!(output.stderr.starts_with(b"speakeasy: "));
    }
    Ok(())
}

#[test]
fn a_control_command_without_a_settings_folder_is_unknown_rather_than_refused() -> anyhow::Result<()>
{
    let output = Command::new(APP)
        .arg("status")
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("APPDATA")
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stderr, b"");
    let reply: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(reply["result"], "unknown");
    assert!(
        reply["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("settings folder"))
    );
    Ok(())
}
