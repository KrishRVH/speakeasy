use std::io::Cursor;

use anyhow::Context as _;

use super::{super::DISCLOSURE, *};

fn stateless(id: Value, method: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": {"_meta": {VERSION_META: STATELESS_VERSION}}})
}

fn initialize() -> Value {
    json!({"jsonrpc": "2.0", "id": "init", "method": "initialize", "params": {"protocolVersion": INITIALIZED_VERSION, "capabilities": {}, "clientInfo": {"name": "fixture", "version": "1"}}})
}

fn exchange(input: &[u8]) -> anyhow::Result<Vec<Value>> {
    let root = tempfile::tempdir()?;
    let roots = Roots::open(&[root.path().to_owned()], None).map_err(anyhow::Error::msg)?;
    let mut output = Vec::new();
    serve(&roots, &mut Cursor::new(input), &mut output)?;
    output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| Ok(serde_json::from_slice(line)?))
        .collect()
}

fn lines(requests: &[Value]) -> anyhow::Result<Vec<Value>> {
    let mut input = Vec::new();
    for request in requests {
        serde_json::to_writer(&mut input, request)?;
        input.push(b'\n');
    }
    exchange(&input)
}

#[test]
fn stateless_requests_name_their_version_and_unsupported_ones_are_refused() -> anyhow::Result<()> {
    let mut wrong = stateless(json!(2), "tools/list");
    wrong["params"]["_meta"][VERSION_META] = json!("2099-01-01");
    let replies = lines(&[stateless(json!(1), "tools/list"), wrong])?;
    assert_eq!(
        replies[0]["result"]["tools"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(replies[0]["result"]["resultType"], "complete");
    assert_eq!(replies[1]["error"]["code"], -32022);
    assert_eq!(
        replies[1]["error"]["data"]["supported"],
        json!([STATELESS_VERSION])
    );
    Ok(())
}

#[test]
fn initialized_clients_list_shows_and_notifications_get_no_reply() -> anyhow::Result<()> {
    let replies = lines(&[
        initialize(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": "ping", "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "show_list", "arguments": {}}}),
    ])?;
    assert_eq!(replies.len(), 3);
    assert_eq!(replies[0]["result"]["protocolVersion"], INITIALIZED_VERSION);
    assert_eq!(replies[1]["result"], json!({}));
    assert!(replies[2]["result"].get("resultType").is_none());
    let listed: Value = serde_json::from_str(
        replies[2]["result"]["content"][0]["text"]
            .as_str()
            .context("The listing is text")?,
    )?;
    assert_eq!(listed["shows"], json!([]));
    Ok(())
}

#[test]
fn both_handshakes_carry_the_disclosure_and_the_reading_guide() -> anyhow::Result<()> {
    let replies = lines(&[
        initialize(),
        stateless(json!("discover"), "server/discover"),
    ])?;
    assert_eq!(replies.len(), 2);
    for reply in &replies {
        let (disclosure, guide) = reply["result"]["instructions"]
            .as_str()
            .and_then(|instructions| instructions.split_once("\n\n"))
            .context("Instructions lead with the disclosure")?;
        assert_eq!(disclosure, DISCLOSURE);
        assert!(guide.starts_with("# Read a Speakeasy Show\n"), "{guide}");
        assert!(guide.contains("`show_read`"), "{guide}");
    }
    Ok(())
}

#[test]
fn a_malformed_line_gets_an_error_and_the_next_line_is_served() -> anyhow::Result<()> {
    let mut input = b"{malformed\n".to_vec();
    serde_json::to_writer(&mut input, &stateless(json!(9), "tools/list"))?;
    let replies = exchange(&input)?;
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert_eq!(replies[1]["id"], 9);
    Ok(())
}

#[test]
fn tool_errors_are_results_while_unknown_tools_and_methods_are_protocol_errors()
-> anyhow::Result<()> {
    let mut unknown = stateless(json!(1), "tools/call");
    unknown["params"]["name"] = json!("show_everything");
    let mut outside = stateless(json!(2), "tools/call");
    outside["params"]["name"] = json!("show_read");
    outside["params"]["arguments"] = json!({"show": "outside"});
    let replies = lines(&[unknown, outside, stateless(json!(3), "resources/list")])?;
    assert_eq!(replies[0]["error"]["code"], -32602);
    assert_eq!(replies[1]["result"]["isError"], true);
    assert_eq!(replies[2]["error"]["code"], -32601);
    Ok(())
}
