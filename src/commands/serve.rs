//! `serve` subcommand: run okf as an MCP server over stdio.
//!
//! A thin adapter — the protocol loop and tool catalog live in [`crate::mcp`].
//! This hands the server the bundle root and the standard streams and lets it
//! run until stdin closes.

use std::path::Path;
use std::process::ExitCode;

use crate::error::Result;

/// Serve the bundle at `root` over the Model Context Protocol on stdio.
///
/// Blocks reading JSON-RPC requests from stdin and writing responses to stdout
/// until the client closes the stream, then returns success.
///
/// # Errors
///
/// Returns an error only on an unrecoverable I/O failure on stdin or stdout;
/// per-request and per-tool failures are reported in-band to the client, not
/// returned.
pub(crate) fn run(root: &Path) -> Result<ExitCode> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    crate::mcp::serve(root, stdin.lock(), stdout.lock())?;
    Ok(ExitCode::SUCCESS)
}
