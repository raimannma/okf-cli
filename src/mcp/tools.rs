//! The MCP tool catalog and the mapping from a tool call to a [`Command`].
//!
//! Each okf read/search/mutate command is surfaced as one MCP tool whose name is
//! the command name and whose arguments mirror the command's flags. Building a
//! [`Command`] here and handing it to [`crate::commands::dispatch`] means the MCP
//! front-end runs the exact same logic as the CLI — it is a thin adapter, not a
//! parallel implementation.

use serde_json::{Value, json};

use crate::cli::Command;

/// The MCP tool definitions advertised by `tools/list`, in a stable order.
///
/// Every entry is a JSON object with `name`, `description`, and a JSON-Schema
/// `inputSchema`, matching the MCP `Tool` shape.
// One literal `tool(...)` entry per command: long, but flat and declarative.
#[allow(clippy::too_many_lines)]
pub(crate) fn catalog() -> Vec<Value> {
    vec![
        tool(
            "list",
            "Survey the bundle: list concept IDs and frontmatter (never bodies), \
             with optional filters. The cheap first step before reading anything.",
            json!({
                "type": "object",
                "properties": {
                    "path_prefix": { "type": "string", "description": "Only concepts whose ID starts with this prefix (e.g. `tables/`)." },
                    "type": { "type": "array", "items": { "type": "string" }, "description": "Only concepts whose `type` is one of these (matches any)." },
                    "tag": { "type": "array", "items": { "type": "string" }, "description": "Only concepts carrying every one of these tags (matches all)." },
                    "modified_since": { "type": "string", "description": "Only concepts modified at/after this RFC 3339 datetime or YYYY-MM-DD date." }
                }
            }),
        ),
        tool(
            "get",
            "Fetch one or more concepts, optionally only part of each: frontmatter \
             only, body only, or a single section.",
            json!({
                "type": "object",
                "properties": {
                    "concept_ids": { "type": "array", "items": { "type": "string" }, "description": "Concept IDs — bundle-relative paths without `.md` (e.g. `tables/orders`)." },
                    "frontmatter_only": { "type": "boolean", "description": "Return only the frontmatter." },
                    "body_only": { "type": "boolean", "description": "Return only the markdown body." },
                    "section": { "type": "string", "description": "Return only the content under this heading (e.g. `# Schema`)." }
                },
                "required": ["concept_ids"]
            }),
        ),
        tool(
            "search",
            "Keyword/BM25 search over concept bodies and frontmatter; returns ranked \
             concept IDs with snippets.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "array", "items": { "type": "string" }, "description": "Query terms (string or array of terms)." },
                    "limit": { "type": "integer", "minimum": 0, "description": "Return at most this many hits (0 = no limit). Default 20." },
                    "min_score": { "type": "number", "description": "Drop hits below this BM25 score. Default 0.5." }
                },
                "required": ["query"]
            }),
        ),
        tool(
            "neighbors",
            "A concept's graph neighbors: outbound links and backlinks (\"cited by\"), \
             IDs only. `depth` follows the graph that many hops each direction.",
            json!({
                "type": "object",
                "properties": {
                    "concept_id": { "type": "string", "description": "Concept ID — a bundle-relative path without `.md`." },
                    "depth": { "type": "integer", "minimum": 1, "description": "Hops to follow in each direction. Default 1." }
                },
                "required": ["concept_id"]
            }),
        ),
        tool(
            "context",
            "Assemble a self-contained context slice for a concept: the concept plus \
             every concept it links to, out to `depth` hops along outbound links, as \
             full documents. The differentiator — one blob ready to inject.",
            json!({
                "type": "object",
                "properties": {
                    "concept_id": { "type": "string", "description": "Concept ID — a bundle-relative path without `.md`." },
                    "depth": { "type": "integer", "minimum": 1, "description": "Outbound hops to follow from the concept. Default 1." }
                },
                "required": ["concept_id"]
            }),
        ),
        tool(
            "resolve",
            "Resolve a (possibly relative) markdown link to its canonical concept ID \
             and whether that concept exists.",
            json!({
                "type": "object",
                "properties": {
                    "link": { "type": "string", "description": "The markdown link target (e.g. `/tables/x.md`, `./other.md`)." },
                    "from": { "type": "string", "description": "Resolve a `./`/`../` link as if it appeared in this concept ID." }
                },
                "required": ["link"]
            }),
        ),
        tool(
            "set",
            "Create or update a concept: set frontmatter and body, then write it. \
             Gated on conformance (non-empty `type`) unless `force`. Use `dry_run` to \
             preview a unified diff without writing.",
            json!({
                "type": "object",
                "properties": {
                    "concept_id": { "type": "string", "description": "Concept ID to write — a bundle-relative path without `.md`." },
                    "type": { "type": "string", "description": "Set the `type` frontmatter field." },
                    "title": { "type": "string", "description": "Set the `title` frontmatter field." },
                    "description": { "type": "string", "description": "Set the `description` frontmatter field." },
                    "tag": { "type": "array", "items": { "type": "string" }, "description": "Set the `tags` list (replaces existing)." },
                    "frontmatter": { "type": "object", "description": "Merge these frontmatter keys (typed fields above still win)." },
                    "body": { "type": "string", "description": "The markdown body. Omit to keep an existing concept's body." },
                    "dry_run": { "type": "boolean", "description": "Preview the change as a unified diff; write nothing." },
                    "force": { "type": "boolean", "description": "Write even when the result is not conformant." },
                    "no_reindex": { "type": "boolean", "description": "Do not regenerate affected index.md listings." }
                },
                "required": ["concept_id"]
            }),
        ),
        tool(
            "patch",
            "Replace or append a single section of an existing concept, in place — the \
             rest of the file is left byte-for-byte untouched. Supply the section body, \
             not its `#` heading.",
            json!({
                "type": "object",
                "properties": {
                    "concept_id": { "type": "string", "description": "Concept ID to edit." },
                    "section": { "type": "string", "description": "The section heading to target (e.g. `# Joins`)." },
                    "content": { "type": "string", "description": "The new section body (no `#` heading). Omit/empty clears it." },
                    "append": { "type": "boolean", "description": "Append to the section instead of replacing it." },
                    "dry_run": { "type": "boolean", "description": "Preview the change as a unified diff; write nothing." },
                    "force": { "type": "boolean", "description": "Edit even when the concept is not conformant." }
                },
                "required": ["concept_id", "section"]
            }),
        ),
        tool(
            "link",
            "Add a cross-link from one concept to another as a correct bundle-relative \
             markdown bullet. Idempotent.",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "Concept to add the link to." },
                    "to": { "type": "string", "description": "Concept to link to." },
                    "section": { "type": "string", "description": "Section to place the link under. Default `# Related`." },
                    "text": { "type": "string", "description": "Link text instead of the target's title/id." },
                    "dry_run": { "type": "boolean", "description": "Preview the change as a unified diff; write nothing." },
                    "force": { "type": "boolean", "description": "Edit even when `from` is not conformant." }
                },
                "required": ["from", "to"]
            }),
        ),
        tool(
            "unlink",
            "Remove a cross-link from one concept to another (drops the link bullet).",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "Concept to remove the link from." },
                    "to": { "type": "string", "description": "Link target to remove." },
                    "dry_run": { "type": "boolean", "description": "Preview the change as a unified diff; write nothing." },
                    "force": { "type": "boolean", "description": "Edit even when `from` is not conformant." }
                },
                "required": ["from", "to"]
            }),
        ),
        tool(
            "move",
            "Rename/move a concept and rewrite every inbound link across the bundle. \
             Never overwrites an existing destination.",
            json!({
                "type": "object",
                "properties": {
                    "old_id": { "type": "string", "description": "The concept to move." },
                    "new_id": { "type": "string", "description": "The concept's new id." },
                    "dry_run": { "type": "boolean", "description": "Preview every file change as a unified diff; write nothing." },
                    "force": { "type": "boolean", "description": "Move even when the concept is not conformant." },
                    "no_reindex": { "type": "boolean", "description": "Do not regenerate affected index.md listings." }
                },
                "required": ["old_id", "new_id"]
            }),
        ),
        tool(
            "remove",
            "Delete one or more concepts and scrub every link bullet to them across the \
             bundle.",
            json!({
                "type": "object",
                "properties": {
                    "concept_ids": { "type": "array", "items": { "type": "string" }, "description": "Concept IDs to delete." },
                    "dry_run": { "type": "boolean", "description": "Preview every change as a unified diff; delete nothing." },
                    "force": { "type": "boolean", "description": "Skip ids that do not exist instead of erroring." },
                    "no_reindex": { "type": "boolean", "description": "Do not regenerate affected index.md listings." }
                },
                "required": ["concept_ids"]
            }),
        ),
        tool(
            "log",
            "Append a dated entry to the bundle's reserved log.md change history (under \
             today's UTC date). Supply the prose only.",
            json!({
                "type": "object",
                "properties": {
                    "entry": { "type": "string", "description": "The entry prose to append." },
                    "dry_run": { "type": "boolean", "description": "Preview the change as a unified diff; write nothing." }
                },
                "required": ["entry"]
            }),
        ),
        tool(
            "fmt",
            "Normalize concepts to their canonical on-disk form (frontmatter key order \
             preserved). Previews by default; set `write` to write in place.",
            json!({
                "type": "object",
                "properties": {
                    "concept_ids": { "type": "array", "items": { "type": "string" }, "description": "Concept IDs to format." },
                    "write": { "type": "boolean", "description": "Write the canonical form back to each file." },
                    "force": { "type": "boolean", "description": "Format even a concept with no non-empty `type`." }
                },
                "required": ["concept_ids"]
            }),
        ),
        tool(
            "index",
            "(Re)generate the reserved index.md directory listings across the bundle. \
             `check` is a read-only drift gate; `dry_run` previews diffs.",
            json!({
                "type": "object",
                "properties": {
                    "check": { "type": "boolean", "description": "Report out-of-date listings and fail; write nothing." },
                    "dry_run": { "type": "boolean", "description": "Preview the changes as unified diffs; write nothing." }
                }
            }),
        ),
        tool(
            "check",
            "Validate the whole bundle without changing anything: report broken \
             links (links to a concept absent from the bundle) and files that \
             cannot be parsed as concepts.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
    ]
}

/// Assemble one tool definition object, moving `input_schema` in.
fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".to_owned(), Value::from(name));
    obj.insert("description".to_owned(), Value::from(description));
    obj.insert("inputSchema".to_owned(), input_schema);
    Value::Object(obj)
}

/// Build the [`Command`] for a tool named `name` from its JSON `arguments`.
///
/// Returns a human-readable message (surfaced to the agent as a tool error) when
/// the tool is unknown or its arguments are the wrong shape. Deeper validation —
/// id syntax, conformance, existence — is left to the command itself, so the MCP
/// and CLI paths reject identically.
pub(crate) fn build_command(name: &str, args: &Value) -> Result<Command, String> {
    match name {
        "list" => Ok(Command::List {
            path_prefix: opt_string(args, "path_prefix")?,
            type_: string_list(args, "type")?,
            tag: string_list(args, "tag")?,
            modified_since: opt_string(args, "modified_since")?,
        }),
        "get" => Ok(Command::Get {
            concept_ids: req_string_list(args, "concept_ids")?,
            frontmatter_only: opt_bool(args, "frontmatter_only", false)?,
            body_only: opt_bool(args, "body_only", false)?,
            section: opt_string(args, "section")?,
        }),
        "search" => Ok(Command::Search {
            query: req_string_list(args, "query")?,
            limit: opt_usize(args, "limit", 20)?,
            min_score: opt_f64(args, "min_score", 0.5)?,
        }),
        "neighbors" => Ok(Command::Neighbors {
            concept_id: req_string(args, "concept_id")?,
            depth: opt_usize(args, "depth", 1)?,
        }),
        "context" => Ok(Command::Context {
            concept_id: req_string(args, "concept_id")?,
            depth: opt_usize(args, "depth", 1)?,
        }),
        "resolve" => Ok(Command::Resolve {
            link: req_string(args, "link")?,
            from: opt_string(args, "from")?,
        }),
        "set" => Ok(Command::Set {
            concept_id: req_string(args, "concept_id")?,
            type_: opt_string(args, "type")?,
            title: opt_string(args, "title")?,
            description: opt_string(args, "description")?,
            tag: string_list(args, "tag")?,
            frontmatter: opt_frontmatter(args)?,
            body: opt_string(args, "body")?,
            dry_run: opt_bool(args, "dry_run", false)?,
            force: opt_bool(args, "force", false)?,
            no_reindex: opt_bool(args, "no_reindex", false)?,
        }),
        "patch" => Ok(Command::Patch {
            concept_id: req_string(args, "concept_id")?,
            section: req_string(args, "section")?,
            content: opt_string(args, "content")?,
            append: opt_bool(args, "append", false)?,
            dry_run: opt_bool(args, "dry_run", false)?,
            force: opt_bool(args, "force", false)?,
        }),
        "link" => Ok(Command::Link {
            from: req_string(args, "from")?,
            to: req_string(args, "to")?,
            section: opt_string(args, "section")?.unwrap_or_else(|| "# Related".to_owned()),
            text: opt_string(args, "text")?,
            dry_run: opt_bool(args, "dry_run", false)?,
            force: opt_bool(args, "force", false)?,
        }),
        "unlink" => Ok(Command::Unlink {
            from: req_string(args, "from")?,
            to: req_string(args, "to")?,
            dry_run: opt_bool(args, "dry_run", false)?,
            force: opt_bool(args, "force", false)?,
        }),
        "move" => Ok(Command::Move {
            old_id: req_string(args, "old_id")?,
            new_id: req_string(args, "new_id")?,
            dry_run: opt_bool(args, "dry_run", false)?,
            force: opt_bool(args, "force", false)?,
            no_reindex: opt_bool(args, "no_reindex", false)?,
        }),
        "remove" => Ok(Command::Remove {
            concept_ids: req_string_list(args, "concept_ids")?,
            dry_run: opt_bool(args, "dry_run", false)?,
            force: opt_bool(args, "force", false)?,
            no_reindex: opt_bool(args, "no_reindex", false)?,
        }),
        "log" => Ok(Command::Log {
            append: true,
            entry: Some(req_string(args, "entry")?),
            dry_run: opt_bool(args, "dry_run", false)?,
        }),
        "fmt" => Ok(Command::Fmt {
            concept_ids: req_string_list(args, "concept_ids")?,
            write: opt_bool(args, "write", false)?,
            force: opt_bool(args, "force", false)?,
        }),
        "index" => Ok(Command::Index {
            regenerate: true,
            check: opt_bool(args, "check", false)?,
            dry_run: opt_bool(args, "dry_run", false)?,
        }),
        "check" => Ok(Command::Check {}),
        other => Err(format!("unknown tool `{other}`")),
    }
}

/// The named argument, treating both an absent key and an explicit JSON `null`
/// as "not given".
fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|v| !v.is_null())
}

fn opt_string(args: &Value, key: &str) -> Result<Option<String>, String> {
    match arg(args, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn req_string(args: &Value, key: &str) -> Result<String, String> {
    opt_string(args, key)?.ok_or_else(|| format!("`{key}` is required"))
}

fn opt_bool(args: &Value, key: &str, default: bool) -> Result<bool, String> {
    match arg(args, key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("`{key}` must be a boolean")),
    }
}

/// A list of strings, accepting either a JSON array of strings or a single bare
/// string (so `"query"` and `["query"]` are equivalent). Absent ⇒ empty.
fn string_list(args: &Value, key: &str) -> Result<Vec<String>, String> {
    match arg(args, key) {
        None => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.clone()),
                _ => Err(format!("`{key}` must be a string or array of strings")),
            })
            .collect(),
        Some(_) => Err(format!("`{key}` must be a string or array of strings")),
    }
}

fn req_string_list(args: &Value, key: &str) -> Result<Vec<String>, String> {
    let list = string_list(args, key)?;
    if list.is_empty() {
        return Err(format!("`{key}` is required and must be non-empty"));
    }
    Ok(list)
}

fn opt_usize(args: &Value, key: &str, default: usize) -> Result<usize, String> {
    match arg(args, key) {
        None => Ok(default),
        Some(Value::Number(n)) => {
            let u = n
                .as_u64()
                .ok_or_else(|| format!("`{key}` must be a non-negative integer"))?;
            usize::try_from(u).map_err(|_| format!("`{key}` is too large"))
        }
        Some(_) => Err(format!("`{key}` must be a non-negative integer")),
    }
}

fn opt_f64(args: &Value, key: &str, default: f64) -> Result<f64, String> {
    match arg(args, key) {
        None => Ok(default),
        Some(Value::Number(n)) => n
            .as_f64()
            .ok_or_else(|| format!("`{key}` must be a number")),
        Some(_) => Err(format!("`{key}` must be a number")),
    }
}

/// The `set` frontmatter overlay: accept a JSON object (re-encoded to the JSON
/// string the command parses) or a ready-made string.
fn opt_frontmatter(args: &Value) -> Result<Option<String>, String> {
    match arg(args, "frontmatter") {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(v @ Value::Object(_)) => serde_json::to_string(v)
            .map(Some)
            .map_err(|e| format!("invalid `frontmatter`: {e}")),
        Some(_) => Err("`frontmatter` must be an object or a JSON string".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn catalog_lists_every_command_tool() {
        let names: Vec<String> = catalog()
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect();
        assert_eq!(
            names,
            vec![
                "list",
                "get",
                "search",
                "neighbors",
                "context",
                "resolve",
                "set",
                "patch",
                "link",
                "unlink",
                "move",
                "remove",
                "log",
                "fmt",
                "index",
                "check",
            ]
        );
        // The CLI-only `serve` command must never be exposed as a tool.
        assert!(!names.iter().any(|n| n == "serve"));
    }

    #[test]
    fn build_list_accepts_scalar_and_array_filters() {
        let cmd = build_command("list", &json!({ "type": "Metric", "tag": ["a", "b"] })).unwrap();
        match cmd {
            Command::List { type_, tag, .. } => {
                assert_eq!(type_, vec!["Metric"]);
                assert_eq!(tag, vec!["a", "b"]);
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn build_search_defaults_limit_and_score() {
        let cmd = build_command("search", &json!({ "query": "orders" })).unwrap();
        match cmd {
            Command::Search {
                query,
                limit,
                min_score,
            } => {
                assert_eq!(query, vec!["orders"]);
                assert_eq!(limit, 20);
                assert!((min_score - 0.5).abs() < f64::EPSILON);
            }
            other => panic!("expected search, got {other:?}"),
        }
    }

    #[test]
    fn build_set_encodes_frontmatter_object() {
        let cmd = build_command(
            "set",
            &json!({ "concept_id": "x", "frontmatter": { "owner": "ann" } }),
        )
        .unwrap();
        match cmd {
            Command::Set { frontmatter, .. } => {
                assert_eq!(frontmatter.as_deref(), Some(r#"{"owner":"ann"}"#));
            }
            other => panic!("expected set, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_arg_is_an_error() {
        let err = build_command("get", &json!({})).unwrap_err();
        assert!(err.contains("concept_ids"));
    }

    #[test]
    fn wrong_type_arg_is_an_error() {
        let err = build_command("neighbors", &json!({ "concept_id": 7 })).unwrap_err();
        assert!(err.contains("concept_id"));
    }

    #[test]
    fn unknown_tool_is_an_error() {
        let err = build_command("frobnicate", &json!({})).unwrap_err();
        assert!(err.contains("frobnicate"));
    }

    #[test]
    fn link_defaults_section_to_related() {
        let cmd = build_command("link", &json!({ "from": "a", "to": "b" })).unwrap();
        match cmd {
            Command::Link { section, .. } => assert_eq!(section, "# Related"),
            other => panic!("expected link, got {other:?}"),
        }
    }
}
