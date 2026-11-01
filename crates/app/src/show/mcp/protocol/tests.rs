use std::io::Cursor;

use super::*;

fn modern(id: Value, method: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": {"_meta": {VERSION_META: STATELESS_VERSION, CAPABILITIES_META: {}}}})
}

fn exchange(lines: &[Value]) -> anyhow::Result<Vec<Value>> {
    let root = tempfile::tempdir()?;
    let roots = Roots::open(vec![root.path().to_owned()]).map_err(anyhow::Error::msg)?;
    let mut input = Vec::new();
    for line in lines {
        serde_json::to_writer(&mut input, line)?;
        writeln!(input)?;
    }
    let mut output = Vec::new();
    serve(&roots, &mut Cursor::new(input), &mut output)?;
    output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| Ok(serde_json::from_slice(line)?))
        .collect()
}

#[test]
fn modern_requests_are_stateless_and_each_requires_metadata() -> anyhow::Result<()> {
    let mut missing = modern(json!(3), "tools/list");
    missing["params"]["_meta"] = json!({VERSION_META: STATELESS_VERSION});
    let mut wrong = modern(json!(4), "tools/list");
    wrong["params"]["_meta"][VERSION_META] = json!("2099-01-01");
    let replies = exchange(&[
        modern(json!(1), "tools/list"),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        missing,
        wrong,
        modern(json!(5), "server/discover"),
    ])?;
    assert_eq!(replies.len(), 5);
    assert_eq!(replies[0]["result"]["tools"].as_array().unwrap().len(), 3);
    assert_eq!(replies[0]["result"]["resultType"], "complete");
    assert_eq!(replies[0]["result"]["cacheScope"], "private");
    assert_eq!(replies[0]["result"]["ttlMs"], 0);
    assert_eq!(replies[1]["error"]["code"], -32602);
    assert_eq!(replies[2]["error"]["code"], -32602);
    assert_eq!(replies[3]["error"]["code"], -32022);
    assert_eq!(replies[3]["error"]["data"]["requested"], "2099-01-01");
    assert_eq!(
        replies[3]["error"]["data"]["supported"],
        json!([STATELESS_VERSION])
    );
    assert_eq!(
        replies[4]["result"]["supportedVersions"],
        json!([STATELESS_VERSION, INITIALIZED_VERSION])
    );
    assert!(
        replies[4]["result"]["capabilities"]
            .get("resources")
            .is_none()
    );
    Ok(())
}

#[test]
fn initialized_clients_negotiate_a_supported_version_and_notifications_are_silent()
-> anyhow::Result<()> {
    let replies = exchange(&[
        json!({"jsonrpc": "2.0", "id": "init", "method": "initialize", "params": {"protocolVersion": "unknown", "capabilities": {}, "clientInfo": {"name": "fixture", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "method": "tools/call", "params": {"name": "show_read", "arguments": {"show": "outside"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 12}}),
        json!({"jsonrpc": "2.0", "id": "ping", "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "show_list", "arguments": {}}}),
    ])?;
    assert_eq!(replies.len(), 5);
    assert_eq!(replies[0]["result"]["protocolVersion"], INITIALIZED_VERSION);
    assert_eq!(replies[1]["error"]["code"], -32602);
    assert_eq!(replies[2]["result"], json!({}));
    assert!(replies[3]["result"].get("resultType").is_none());
    assert_eq!(replies[4]["result"]["isError"], false);
    let listed: Value =
        serde_json::from_str(replies[4]["result"]["content"][0]["text"].as_str().unwrap())?;
    assert_eq!(listed["shows"], json!([]));
    Ok(())
}

#[test]
fn malformed_oversized_and_invalid_requests_recover_at_the_next_line() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let roots = Roots::open(vec![root.path().to_owned()]).map_err(anyhow::Error::msg)?;
    let mut input = b"{private malformed\n".to_vec();
    input.extend(std::iter::repeat_n(b'x', LINE_LIMIT + 300));
    input.extend_from_slice(b"\n");
    for line in [
        json!([]),
        json!({"jsonrpc": "2.0", "id": null, "method": "tools/list"}),
        modern(json!(9), "tools/list"),
    ] {
        serde_json::to_writer(&mut input, &line)?;
        writeln!(input)?;
    }
    let mut output = Vec::new();
    serve(
        &roots,
        &mut io::BufReader::with_capacity(7, Cursor::new(input)),
        &mut output,
    )?;
    let output = String::from_utf8(output)?;
    assert!(!output.contains("private malformed"));
    let replies: Vec<Value> = output
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(replies.len(), 5);
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert_eq!(replies[1]["error"]["code"], -32700);
    assert_eq!(replies[2]["error"]["code"], -32600);
    assert_eq!(replies[3]["error"]["code"], -32600);
    assert_eq!(replies[4]["id"], 9);
    Ok(())
}

#[test]
fn tool_errors_are_distinct_from_protocol_errors_and_never_echo_arguments() -> anyhow::Result<()> {
    let mut unknown = modern(json!(1), "tools/call");
    unknown["params"]["name"] = json!("PRIVATE_UNKNOWN_TOOL");
    let mut invalid = modern(json!(2), "tools/call");
    invalid["params"]["name"] = json!("show_read");
    invalid["params"]["arguments"] = json!({"show": "PRIVATE_OUTSIDE", "private": "PRIVATE_BODY"});
    let replies = exchange(&[unknown, invalid, modern(json!(3), "resources/list")])?;
    assert_eq!(replies[0]["error"]["code"], -32602);
    assert_eq!(replies[1]["result"]["isError"], true);
    assert_eq!(replies[2]["error"]["code"], -32601);
    assert!(!serde_json::to_string(&replies)?.contains("PRIVATE_"));
    Ok(())
}

#[test]
fn output_is_bounded_and_a_closed_client_ends_the_server() -> anyhow::Result<()> {
    struct Closed;
    impl io::Write for Closed {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let root = tempfile::tempdir()?;
    let roots = Roots::open(vec![root.path().to_owned()]).map_err(anyhow::Error::msg)?;
    let input = serde_json::to_vec(&modern(json!(1), "server/discover"))?;
    serve(&roots, &mut Cursor::new(input), &mut Closed)?;
    assert!(encode(&"x".repeat(RESPONSE_LIMIT)).is_err());
    Ok(())
}
