use std::io::{self, BufRead, Write};

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::{Roots, instructions, tools};

const STATELESS_VERSION: &str = "2026-07-28";
const INITIALIZED_VERSION: &str = "2025-11-25";
const VERSION_META: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_META: &str = "io.modelcontextprotocol/clientCapabilities";
const LINE_LIMIT: usize = 64 << 10;
const RESPONSE_LIMIT: usize = 12 << 20;

const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;
const UNSUPPORTED_VERSION: i32 = -32022;

#[derive(Default)]
enum Protocol {
    #[default]
    New,
    Initializing,
    Ready,
}

pub(super) fn serve(
    roots: &Roots,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> io::Result<()> {
    let mut protocol = Protocol::default();
    loop {
        let response = match read_line(input)? {
            Line::Eof => return Ok(()),
            Line::Message(line) => match serde_json::from_slice::<Value>(&line) {
                Ok(value) => protocol.respond(roots, value),
                Err(_private) => Some(error(Value::Null, PARSE_ERROR, "Invalid JSON", None)),
            },
            Line::Oversized => Some(error(
                Value::Null,
                PARSE_ERROR,
                "MCP input line exceeds 64 KiB",
                None,
            )),
        };
        if let Some(response) = response {
            let encoded = match encode(&response, RESPONSE_LIMIT) {
                Ok(encoded) => encoded,
                Err(_private) => encode(
                    &error(
                        response.get("id").cloned().unwrap_or(Value::Null),
                        INTERNAL_ERROR,
                        "MCP response exceeds the output limit",
                        None,
                    ),
                    RESPONSE_LIMIT,
                )?,
            };
            if let Err(error) = out
                .write_all(&encoded)
                .and_then(|()| out.write_all(b"\n"))
                .and_then(|()| out.flush())
            {
                return if error.kind() == io::ErrorKind::BrokenPipe {
                    Ok(())
                } else {
                    Err(error)
                };
            }
        }
    }
}

enum Line {
    Eof,
    Oversized,
    Message(Vec<u8>),
}

fn read_line(input: &mut impl BufRead) -> io::Result<Line> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return Ok(if oversized {
                Line::Oversized
            } else if line.is_empty() {
                Line::Eof
            } else {
                Line::Message(line)
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let length = newline.unwrap_or(available.len());
        if !oversized {
            if line.len().saturating_add(length) > LINE_LIMIT {
                oversized = true;
                line.clear();
            } else if let Some(bytes) = available.get(..length) {
                line.extend_from_slice(bytes);
            }
        }
        input.consume(length.saturating_add(usize::from(newline.is_some())));
        if newline.is_some() {
            return Ok(if oversized {
                Line::Oversized
            } else {
                Line::Message(line)
            });
        }
    }
}

impl Protocol {
    fn respond(&mut self, roots: &Roots, mut value: Value) -> Option<Value> {
        let Some(request) = value.as_object() else {
            return Some(error(
                Value::Null,
                INVALID_REQUEST,
                "Expected one JSON-RPC request object",
                None,
            ));
        };
        let id = request.get("id");
        let valid_id = id.filter(|id| {
            id.as_str().is_some_and(|id| id.len() <= 1_024) || id.is_i64() || id.is_u64()
        });
        let Some(method) = request.get("method").and_then(Value::as_str).filter(|_| {
            request.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                && (id.is_none() || valid_id.is_some())
        }) else {
            return Some(error(
                valid_id.cloned().unwrap_or(Value::Null),
                INVALID_REQUEST,
                "Invalid JSON-RPC request",
                None,
            ));
        };
        let Some(id) = valid_id.cloned() else {
            if method == "notifications/initialized" && !matches!(self, Self::New) {
                *self = Self::Ready;
            }
            return None;
        };
        let empty = Map::new();
        let params = match request.get("params") {
            None => &empty,
            Some(Value::Object(params)) => params,
            Some(_) => {
                return Some(error(
                    id,
                    INVALID_PARAMS,
                    "Request parameters must be an object",
                    None,
                ));
            },
        };
        if method == "initialize" {
            return Some(self.initialize(id, params));
        }
        let stateless = params.get("_meta").is_some_and(|meta| {
            meta.get(VERSION_META).is_some() || meta.get(CAPABILITIES_META).is_some()
        });
        if method == "ping" && !stateless && !matches!(self, Self::New) {
            return Some(success(id, json!({}), false));
        }
        if (stateless || !matches!(self, Self::Ready) || method == "server/discover")
            && let Err(failure) = metadata(params)
        {
            return Some(error(id, failure.code, failure.message, failure.data));
        }
        let result = match method {
            "server/discover" => json!({
                "supportedVersions": [STATELESS_VERSION, INITIALIZED_VERSION], "capabilities": {"tools": {}},
                "_meta": {"io.modelcontextprotocol/serverInfo": server_info()},
                "instructions": instructions(), "ttlMs": 0, "cacheScope": "private"
            }),
            "tools/list" => {
                if params.get("cursor").is_some() {
                    return Some(error(
                        id,
                        INVALID_PARAMS,
                        "This tool list has no next page",
                        None,
                    ));
                }
                let mut result = Map::from_iter([("tools".into(), tools::definitions())]);
                if stateless {
                    result.insert("ttlMs".into(), json!(0));
                    result.insert("cacheScope".into(), json!("private"));
                }
                Value::Object(result)
            },
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(error(id, INVALID_PARAMS, "A tool name is required", None));
                };
                let Some(tool) = tools::Tool::parse(name) else {
                    return Some(error(id, INVALID_PARAMS, "Unknown tool", None));
                };
                let arguments = value
                    .get_mut("params")
                    .and_then(|params| params.get_mut("arguments"))
                    .map_or_else(|| json!({}), Value::take);
                tools::call(roots, tool, arguments)
            },
            _ => return Some(error(id, METHOD_NOT_FOUND, "Method not found", None)),
        };
        Some(success(id, result, stateless))
    }

    fn initialize(&mut self, id: Value, params: &Map<String, Value>) -> Value {
        if !matches!(self, Self::New) {
            return error(
                id,
                INVALID_REQUEST,
                "MCP initialization is already complete",
                None,
            );
        }
        if !params.get("protocolVersion").is_some_and(Value::is_string)
            || !params.get("capabilities").is_some_and(Value::is_object)
            || !params.get("clientInfo").is_some_and(|info| {
                info.get("name").is_some_and(Value::is_string)
                    && info.get("version").is_some_and(Value::is_string)
            })
        {
            return error(
                id,
                INVALID_PARAMS,
                "Initialize needs protocolVersion, capabilities and clientInfo",
                None,
            );
        }
        *self = Self::Initializing;
        success(
            id,
            json!({"protocolVersion": INITIALIZED_VERSION, "capabilities": {"tools": {}}, "serverInfo": server_info(), "instructions": instructions()}),
            false,
        )
    }
}

struct MetadataError {
    code: i32,
    message: &'static str,
    data: Option<Value>,
}

fn metadata(params: &Map<String, Value>) -> Result<(), MetadataError> {
    let meta = params.get("_meta");
    let version = meta
        .and_then(|meta| meta.get(VERSION_META))
        .and_then(Value::as_str);
    let capabilities = meta.and_then(|meta| meta.get(CAPABILITIES_META));
    let Some(version) = version.filter(|version| version.len() <= 64) else {
        return Err(MetadataError {
            code: INVALID_PARAMS,
            message: "Each modern request needs protocolVersion and clientCapabilities metadata",
            data: None,
        });
    };
    if version != STATELESS_VERSION {
        return Err(MetadataError {
            code: UNSUPPORTED_VERSION,
            message: "Unsupported protocol version",
            data: Some(json!({"supported": [STATELESS_VERSION], "requested": version})),
        });
    }
    if !capabilities.is_some_and(Value::is_object) {
        return Err(MetadataError {
            code: INVALID_PARAMS,
            message: "Each modern request needs clientCapabilities metadata",
            data: None,
        });
    }
    Ok(())
}

fn server_info() -> Value {
    json!({"name": "speakeasy-show", "version": env!("CARGO_PKG_VERSION")})
}

fn success(id: Value, result: Value, stateless: bool) -> Value {
    let Value::Object(mut result) = result else {
        return error(id, INTERNAL_ERROR, "MCP result must be an object", None);
    };
    if stateless {
        result.insert("resultType".into(), json!("complete"));
    }
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i32, message: &'static str, data: Option<Value>) -> Value {
    let mut error = Map::from_iter([
        ("code".into(), json!(code)),
        ("message".into(), json!(message)),
    ]);
    if let Some(data) = data {
        error.insert("data".into(), data);
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

pub(super) fn encode(value: &impl Serialize, limit: usize) -> io::Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.bytes.len().saturating_add(bytes.len()) > self.limit {
                return Err(io::Error::other("MCP response exceeds the output limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut encoded = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut encoded, value)
        .map_err(|_private| io::Error::other("Cannot encode the bounded MCP response"))?;
    Ok(encoded.bytes)
}

#[cfg(test)]
mod tests;
