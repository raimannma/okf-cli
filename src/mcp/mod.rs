//! The Model Context Protocol front-end.
//!
//! A synchronous stdio server speaking newline-delimited JSON-RPC 2.0 — the MCP
//! stdio transport. It advertises every okf read/search/mutate command as a tool
//! (see [`tools`]) and, for each `tools/call`, builds the corresponding
//! [`crate::cli::Command`] and runs it through [`crate::commands::dispatch`] with
//! a capturing [`OutputMode`]. The CLI and this server are therefore two thin
//! adapters over one core: a tool call executes the exact same code path as the
//! equivalent `okf` invocation.

mod tools;

use std::io::{BufRead, Write};
use std::path::Path;

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::output::OutputMode;

/// The MCP revision we default to when a client does not announce one. We echo
/// the client's requested version when it sends one, so negotiation still works.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

// JSON-RPC 2.0 error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Serve the bundle at `root` over MCP, reading requests from `reader` and
/// writing responses to `writer` until the input stream closes.
///
/// One JSON-RPC message per line, one response line per request; notifications
/// (no `id`) get no response. Tool-level failures are returned to the client as
/// `isError` tool results, not as transport errors, so the loop runs until EOF.
///
/// # Errors
///
/// [`Error::McpIo`] if a line cannot be read from `reader` or a response cannot
/// be written/flushed to `writer`.
pub(crate) fn serve(root: &Path, mut reader: impl BufRead, mut writer: impl Write) -> Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|source| Error::McpIo { source })?;
        if read == 0 {
            return Ok(()); // EOF: client closed the stream.
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(response) = handle_message(root, trimmed) {
            write_message(&mut writer, &response)?;
        }
    }
}

/// Serialize one JSON-RPC message and write it as a single line, flushing so the
/// client sees the response immediately.
fn write_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    let line = serde_json::to_string(message).map_err(Error::Json)?;
    writer
        .write_all(line.as_bytes())
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush())
        .map_err(|source| Error::McpIo { source })
}

/// Parse and route one request line, returning the response to send — or `None`
/// for a notification, which JSON-RPC answers with silence.
fn handle_message(root: &Path, line: &str) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return Some(rpc_error(Value::Null, PARSE_ERROR, "Parse error")),
    };
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned();

    match (method, id) {
        // A request (has id) but no method: malformed.
        (None, Some(id)) => Some(rpc_error(
            id,
            INVALID_REQUEST,
            "Invalid Request: missing method",
        )),
        // A notification (no id): nothing to answer, with or without a method.
        (_, None) => None,
        (Some(method), Some(id)) => Some(route(root, method, message.get("params"), id)),
    }
}

/// Build the response for a request `method` with the given `params` and `id`.
fn route(root: &Path, method: &str, params: Option<&Value>, id: Value) -> Value {
    match method {
        "initialize" => rpc_success(id, initialize_result(params)),
        "ping" => rpc_success(id, json!({})),
        "tools/list" => rpc_success(id, json!({ "tools": tools::catalog() })),
        "tools/call" => match call_tool(root, params) {
            Ok(result) => rpc_success(id, result),
            Err((code, message)) => rpc_error(id, code, &message),
        },
        other => rpc_error(id, METHOD_NOT_FOUND, &format!("Method not found: {other}")),
    }
}

/// The `initialize` result: capabilities and identity, echoing the client's
/// protocol version when it offered one.
fn initialize_result(params: Option<&Value>) -> Value {
    let version = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "okf", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// Validate a `tools/call` request and run the named tool.
///
/// `Err((code, message))` is a JSON-RPC protocol fault (bad `params` shape);
/// `Ok(result)` is a `CallToolResult`, which itself reports tool-level failures
/// via `isError` rather than a transport error.
fn call_tool(root: &Path, params: Option<&Value>) -> std::result::Result<Value, (i64, String)> {
    let params = params.ok_or((INVALID_PARAMS, "tools/call requires params".to_owned()))?;
    let name = params.get("name").and_then(Value::as_str).ok_or((
        INVALID_PARAMS,
        "tools/call requires a string `name`".to_owned(),
    ))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Ok(run_tool(root, name, &arguments))
}

/// Build the command for `name`, run it with a capturing output mode, and wrap
/// the captured result (or the failure) as an MCP `CallToolResult`.
fn run_tool(root: &Path, name: &str, arguments: &Value) -> Value {
    let command = match tools::build_command(name, arguments) {
        Ok(command) => command,
        Err(message) => return tool_error_text(&message),
    };

    let command_name = command.name();
    // Clear any stale capture before running, then read this command's result.
    let _ = crate::output::take_captured();
    let mode = OutputMode::capturing(command_name);

    match crate::commands::dispatch(command, root, mode) {
        Ok(_exit_code) => tool_success(command_name, crate::output::take_captured()),
        Err(err) => tool_error(&err),
    }
}

/// A successful `CallToolResult`: the captured envelope as both pretty-printed
/// text content (for display) and `structuredContent` (for machine consumers).
fn tool_success(command: &str, captured: Option<crate::output::Captured>) -> Value {
    let (data, warnings) = match captured {
        Some(c) => (c.data, c.warnings),
        None => (Value::Null, Vec::new()),
    };
    let envelope = json!({
        "command": command,
        "ok": true,
        "data": data,
        "warnings": warnings,
    });
    let text = serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| envelope.to_string());
    json!({
        "content": [ { "type": "text", "text": text } ],
        "structuredContent": envelope,
    })
}

/// An error `CallToolResult` from a typed command failure, carrying the message,
/// its cause chain, and the command's exit code.
fn tool_error(err: &Error) -> Value {
    json!({
        "content": [ { "type": "text", "text": crate::output::error_text(err) } ],
        "isError": true,
        "structuredContent": {
            "ok": false,
            "error": crate::output::ErrorPayload::for_error(err),
        },
    })
}

/// An error `CallToolResult` from a plain message (bad arguments / unknown tool).
fn tool_error_text(message: &str) -> Value {
    json!({
        "content": [ { "type": "text", "text": format!("error: {message}") } ],
        "isError": true,
    })
}

/// A JSON-RPC success response. Builds the object by hand so `id`/`result` move
/// into it rather than being cloned, as the `json!` macro would.
fn rpc_success(id: Value, result: Value) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("jsonrpc".to_owned(), Value::from("2.0"));
    obj.insert("id".to_owned(), id);
    obj.insert("result".to_owned(), result);
    Value::Object(obj)
}

/// A JSON-RPC error response. Builds the object by hand so `id` moves in rather
/// than being cloned, as the `json!` macro would.
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    let mut error = serde_json::Map::new();
    error.insert("code".to_owned(), Value::from(code));
    error.insert("message".to_owned(), Value::from(message));
    let mut obj = serde_json::Map::new();
    obj.insert("jsonrpc".to_owned(), Value::from("2.0"));
    obj.insert("id".to_owned(), id);
    obj.insert("error".to_owned(), Value::Object(error));
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Drive the server with a canned set of request lines and return the
    /// response lines it wrote, each parsed back into a `Value`.
    fn exchange(root: &Path, requests: &[Value]) -> Vec<Value> {
        let input = requests
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let mut output = Vec::new();
        serve(root, input.as_bytes(), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn bundle() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("orders.md"),
            "---\ntype: Table\ntitle: Orders\n---\n# Schema\nid, total\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn initialize_echoes_protocol_and_advertises_tools() {
        let dir = bundle();
        let out = exchange(
            dir.path(),
            &[
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                        "params": { "protocolVersion": "2025-06-18" } }),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            ],
        );
        // The notification produced no response, so only two lines come back.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(out[0]["result"]["serverInfo"]["name"], "okf");
        let tools = out[1]["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 16);
    }

    #[test]
    fn tools_call_list_returns_captured_envelope() {
        let dir = bundle();
        let out = exchange(
            dir.path(),
            &[json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                     "params": { "name": "list", "arguments": {} } })],
        );
        let structured = &out[0]["result"]["structuredContent"];
        assert_eq!(structured["command"], "list");
        assert_eq!(structured["ok"], true);
        assert_eq!(structured["data"]["total"], 1);
        assert_eq!(structured["data"]["concepts"][0]["id"], "orders");
    }

    #[test]
    fn tools_call_get_section_scopes_the_read() {
        let dir = bundle();
        let out = exchange(
            dir.path(),
            &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                     "params": { "name": "get",
                                 "arguments": { "concept_ids": ["orders"], "section": "# Schema" } } })],
        );
        let body = out[0]["result"]["structuredContent"]["data"]["concepts"][0]["body"]
            .as_str()
            .unwrap();
        assert!(body.contains("id, total"));
    }

    #[test]
    fn tools_call_set_then_get_round_trips_a_write() {
        let dir = bundle();
        let out = exchange(
            dir.path(),
            &[
                json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": { "name": "set",
                                    "arguments": { "concept_id": "metrics/dau", "type": "Metric",
                                                   "title": "Daily Active Users", "body": "# Definition\nCount.\n" } } }),
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                        "params": { "name": "get", "arguments": { "concept_ids": ["metrics/dau"], "frontmatter_only": true } } }),
            ],
        );
        assert_ne!(out[0]["result"]["isError"], true);
        assert!(dir.path().join("metrics/dau.md").exists());
        let fm = &out[1]["result"]["structuredContent"]["data"]["concepts"][0]["frontmatter"];
        assert_eq!(fm["type"], "Metric");
        assert_eq!(fm["title"], "Daily Active Users");
    }

    #[test]
    fn dry_run_set_writes_nothing() {
        let dir = bundle();
        exchange(
            dir.path(),
            &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                     "params": { "name": "set",
                                 "arguments": { "concept_id": "ghost", "type": "Note", "body": "x", "dry_run": true } } })],
        );
        assert!(!dir.path().join("ghost.md").exists());
    }

    #[test]
    fn tool_failure_is_reported_as_iserror_not_transport_error() {
        let dir = bundle();
        let out = exchange(
            dir.path(),
            &[json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                     "params": { "name": "get", "arguments": { "concept_ids": ["does/not/exist"] } } })],
        );
        // `get` of a missing id resolves: the concept is absent but the call
        // still succeeds, with the failure carried in `data.errors`.
        let errors = out[0]["result"]["structuredContent"]["data"]["errors"]
            .as_array()
            .unwrap();
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn bad_arguments_are_an_iserror_result() {
        let dir = bundle();
        let out = exchange(
            dir.path(),
            &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                     "params": { "name": "neighbors", "arguments": {} } })],
        );
        assert_eq!(out[0]["result"]["isError"], true);
    }

    #[test]
    fn unknown_method_is_a_jsonrpc_error() {
        let dir = bundle();
        let out = exchange(
            dir.path(),
            &[json!({ "jsonrpc": "2.0", "id": 1, "method": "no/such/method" })],
        );
        assert_eq!(out[0]["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn malformed_json_yields_a_parse_error() {
        let dir = bundle();
        let mut output = Vec::new();
        serve(dir.path(), &b"{ not json\n"[..], &mut output).unwrap();
        let response: Value =
            serde_json::from_slice(output.split(|&b| b == b'\n').next().unwrap()).unwrap();
        assert_eq!(response["error"]["code"], PARSE_ERROR);
    }
}
