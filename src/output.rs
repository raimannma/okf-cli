//! The output contract shared by every command.
//!
//! Two rules hold everywhere: structured JSON is opt-in via `--format json` and
//! is the machine contract; self-describing text is the default. Diagnostics and
//! errors go to stderr, results to stdout, so piping `okf … | …` only ever sees
//! the result. Exit codes come from [`crate::error::Error::exit_code`].
//!
//! Every JSON result is one **envelope** so a harness can parse them uniformly:
//!
//! ```json
//! { "command": "list", "ok": true, "data": { … }, "warnings": [ … ] }
//! ```
//!
//! On a hard failure the same envelope carries `ok: false` and an `error` object
//! instead of `data`. `warnings` is non-fatal bundle-load trouble (files skipped
//! while loading) — never a reason to fail, and omitted entirely when empty.

use std::cell::RefCell;

use serde::Serialize;

use crate::cli::{Cli, Format};
use crate::core::Bundle;
use crate::error::Error;

/// The structured result a command emitted, captured in-process instead of
/// printed. The MCP server runs a command with a capturing [`OutputMode`] and
/// reads the result back out via [`take_captured`].
#[derive(Debug)]
pub(crate) struct Captured {
    /// The command's `data` payload, serialized to JSON.
    pub data: serde_json::Value,
    /// The non-fatal load warnings the command would have shown.
    pub warnings: Vec<String>,
}

thread_local! {
    /// The single in-flight captured result. The MCP server is synchronous and
    /// runs one tool at a time, so one slot per thread suffices; using a
    /// thread-local keeps [`OutputMode`] `Copy` and the command signatures
    /// untouched.
    static CAPTURE: RefCell<Option<Captured>> = const { RefCell::new(None) };
}

/// Take and clear the captured result for the current thread, if any.
pub(crate) fn take_captured() -> Option<Captured> {
    CAPTURE.with(|slot| slot.borrow_mut().take())
}

/// How a command should render its output.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OutputMode {
    format: Format,
    quiet: bool,
    /// The running command's name, stamped into the JSON envelope.
    command: &'static str,
    /// When true, [`emit`](Self::emit) captures the result into the thread-local
    /// [`CAPTURE`] slot rather than printing it — the seam the MCP front-end
    /// reuses every command through.
    capture: bool,
}

impl OutputMode {
    /// Derive the output mode from parsed CLI flags and the dispatched command.
    pub(crate) fn from_cli(cli: &Cli, command: &'static str) -> Self {
        Self {
            format: cli.effective_format(),
            quiet: cli.quiet,
            command,
            capture: false,
        }
    }

    /// An output mode that captures a command's structured result into the
    /// thread-local [`CAPTURE`] slot instead of printing it. Used by the MCP
    /// server to run every command through the same
    /// [`crate::commands::dispatch`] path the CLI uses; read the result back
    /// with [`take_captured`].
    pub(crate) fn capturing(command: &'static str) -> Self {
        Self {
            format: Format::Json,
            quiet: true,
            command,
            capture: true,
        }
    }

    /// Construct an output mode directly, for tests that drive a command's
    /// `run` end to end.
    #[cfg(test)]
    pub(crate) fn test(format: Format, quiet: bool, command: &'static str) -> Self {
        Self {
            format,
            quiet,
            command,
            capture: false,
        }
    }

    /// Whether the result is rendered as a structured envelope (JSON or TOON)
    /// rather than human text. Drives where per-item failures go: into the
    /// envelope in structured mode, onto stderr in text mode.
    pub(crate) fn is_structured(&self) -> bool {
        if self.capture {
            return true;
        }
        match self.format {
            Format::Text => false,
            Format::Json => true,
            #[cfg(feature = "toon")]
            Format::Toon => true,
        }
    }

    /// Emit a successful result: the structured envelope in JSON/TOON mode,
    /// otherwise the pre-rendered human `text` on stdout followed by any
    /// `warnings` on stderr.
    pub(crate) fn emit<T: Serialize>(&self, data: &T, text: &str, warnings: &[String]) {
        if self.capture {
            // Serializing a command's already-structured data into a Value
            // cannot realistically fail; capture null rather than losing the
            // result on the off chance it does.
            let data = serde_json::to_value(data).unwrap_or(serde_json::Value::Null);
            CAPTURE.with(|slot| {
                *slot.borrow_mut() = Some(Captured {
                    data,
                    warnings: warnings.to_vec(),
                });
            });
            return;
        }
        let envelope = Envelope {
            command: self.command,
            ok: true,
            data: Some(data),
            error: None,
            warnings,
        };
        match self.format {
            Format::Text => {
                print!("{text}");
                self.emit_warnings(warnings);
            }
            Format::Json => print_json(&envelope),
            #[cfg(feature = "toon")]
            Format::Toon => print_toon(&envelope),
        }
    }

    /// Print non-fatal `warnings` to stderr (one `warning:` line each), unless
    /// `--quiet` was given. JSON mode never uses this — warnings ride the envelope.
    fn emit_warnings(&self, warnings: &[String]) {
        if self.quiet {
            return;
        }
        for warning in warnings {
            eprintln!("warning: {warning}");
        }
    }

    /// Print an error and its cause chain, honoring the output format.
    ///
    /// In structured (JSON/TOON) mode the error becomes a single envelope object
    /// with `ok: false`, so a harness parses success and failure the same way;
    /// otherwise it is the human cause-chain rendering on stderr.
    pub(crate) fn report_error(&self, err: &Error) {
        // The text path needs no envelope, so build it only for the structured
        // formats — that also avoids walking the cause chain twice.
        let envelope = || Envelope::<()> {
            command: self.command,
            ok: false,
            data: None,
            error: Some(ErrorPayload::for_error(err)),
            warnings: &[],
        };
        match self.format {
            Format::Text => eprintln!("{}", error_text(err)),
            // Serializing strings into a fixed object cannot realistically
            // fail; fall back to the plain message rather than swallowing it.
            Format::Json => match serde_json::to_string(&envelope()) {
                Ok(line) => eprintln!("{line}"),
                Err(_) => eprintln!("error: {err}"),
            },
            #[cfg(feature = "toon")]
            Format::Toon => match toon_format::encode_default(&envelope()) {
                Ok(text) => eprintln!("{text}"),
                Err(_) => eprintln!("error: {err}"),
            },
        }
    }
}

/// The single JSON shape every command's result takes.
#[derive(Serialize)]
struct Envelope<'a, T: Serialize> {
    command: &'a str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<&'a T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorPayload>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    warnings: &'a [String],
}

/// The `error` member of a failure envelope, also reused by the MCP front-end so
/// the failure shape is identical across CLI and tool calls.
#[derive(Serialize)]
pub(crate) struct ErrorPayload {
    message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    causes: Vec<String>,
    exit_code: u8,
}

impl ErrorPayload {
    /// The payload for a typed error: its message, cause chain, and exit code.
    pub(crate) fn for_error(err: &Error) -> Self {
        Self {
            message: err.to_string(),
            causes: cause_chain(err),
            exit_code: err.exit_code(),
        }
    }
}

/// Pretty-print an envelope to stdout, falling back to a one-line error on the
/// vanishingly unlikely serialization failure.
fn print_json<T: Serialize>(envelope: &Envelope<'_, T>) {
    match serde_json::to_string_pretty(envelope) {
        Ok(json) => println!("{json}"),
        Err(err) => tracing::error!(%err, "failed to serialize JSON output"),
    }
}

/// Encode an envelope to TOON and print it to stdout, logging the unlikely
/// serialization failure rather than panicking.
#[cfg(feature = "toon")]
fn print_toon<T: Serialize>(envelope: &Envelope<'_, T>) {
    match toon_format::encode_default(envelope) {
        Ok(toon) => println!("{toon}"),
        Err(err) => tracing::error!(%err, "failed to serialize TOON output"),
    }
}

/// The non-fatal load warnings for a bundle: one actionable line per skipped file.
pub(crate) fn bundle_warnings(bundle: &Bundle) -> Vec<String> {
    bundle
        .parse_errors()
        .iter()
        .map(|e| format!("skipped {e}"))
        .collect()
}

/// The messages of an error's underlying `source` chain, outermost cause first.
fn cause_chain(err: &Error) -> Vec<String> {
    let mut causes = Vec::new();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        causes.push(cause.to_string());
        source = cause.source();
    }
    causes
}

/// Render an error and its cause chain as the multi-line text block shared by the
/// CLI's text mode and the MCP tool-error result: `error: …` then one indented
/// `caused by: …` line per cause.
pub(crate) fn error_text(err: &Error) -> String {
    let mut out = format!("error: {err}");
    for cause in cause_chain(err) {
        out.push_str("\n  caused by: ");
        out.push_str(&cause);
    }
    out
}
