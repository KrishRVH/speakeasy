use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

use super::{Roots, instructions, tools};

const STATELESS_VERSION: &str = "2026-07-28";
const INITIALIZED_VERSION: &str = "2025-11-25";
const VERSION_META: &str = "io.modelcontextprotocol/protocolVersion";

const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const UNSUPPORTED_VERSION: i32 = -32022;

pub(super) fn serve(
    roots: &Roots,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> io::Result<()> {
    for line in input.split(b'\n') {
        let response = match serde_json::from_slice::<Value>(&line?) {
            Ok(request) => respond(roots, &request),
            Err(_invalid) => Some(error(Value::Null, PARSE_ERROR, "Invalid JSON", None)),
        };
        let Some(response) = response else {
            continue;
        };
        if let Err(error) = writeln!(out, "{response}").and_then(|()| out.flush()) {
            return if error.kind() == io::ErrorKind::BrokenPipe {
                Ok(())
            } else {
                Err(error)
            };
        }
    }
    Ok(())
}

fn respond(roots: &Roots, request: &Value) -> Option<Value> {
    let id = request
        .get("id")
        .filter(|id| id.is_string() || id.is_number())
        .cloned();
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Some(error(
            id.unwrap_or(Value::Null),
            INVALID_REQUEST,
            "Invalid JSON-RPC request",
            None,
        ));
    };
    let id = id?;
    let params = request.get("params");
    let version = params
        .and_then(|params| params.get("_meta"))
        .and_then(|meta| meta.get(VERSION_META))
        .and_then(Value::as_str);
    if let Some(version) = version
        && version != STATELESS_VERSION
    {
        return Some(error(
            id,
            UNSUPPORTED_VERSION,
            "Unsupported protocol version",
            Some(json!({"supported": [STATELESS_VERSION], "requested": version})),
        ));
    }
    let mut result = match method {
        "initialize" => json!({
            "protocolVersion": INITIALIZED_VERSION, "capabilities": {"tools": {}},
            "serverInfo": server_info(), "instructions": instructions()
        }),
        "ping" => json!({}),
        "server/discover" => json!({
            "supportedVersions": [STATELESS_VERSION, INITIALIZED_VERSION], "capabilities": {"tools": {}},
            "_meta": {"io.modelcontextprotocol/serverInfo": server_info()},
            "instructions": instructions(), "ttlMs": 0, "cacheScope": "private"
        }),
        "tools/list" => json!({"tools": tools::definitions(), "ttlMs": 0, "cacheScope": "private"}),
        "tools/call" => {
            let Some(tool) = params
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .and_then(tools::Tool::parse)
            else {
                return Some(error(id, INVALID_PARAMS, "Unknown tool", None));
            };
            let arguments = params
                .and_then(|params| params.get("arguments"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            tools::call(roots, tool, arguments)
        },
        _ => return Some(error(id, METHOD_NOT_FOUND, "Method not found", None)),
    };
    if version.is_some()
        && let Some(result) = result.as_object_mut()
    {
        result.insert("resultType".into(), json!("complete"));
    }
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn server_info() -> Value {
    json!({"name": "speakeasy-show", "version": env!("CARGO_PKG_VERSION")})
}

fn error(id: Value, code: i32, message: &'static str, data: Option<Value>) -> Value {
    let mut error = json!({"code": code, "message": message});
    if let (Some(data), Some(fields)) = (data, error.as_object_mut()) {
        fields.insert("data".into(), data);
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

#[cfg(test)]
mod tests;
