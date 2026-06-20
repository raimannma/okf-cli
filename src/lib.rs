//! Library entry point.
//!
//! The binary in `main.rs` is a thin shell over [`run`]; keeping the logic here
//! makes it testable and reusable. [`run`] owns the full lifecycle — parse,
//! dispatch, and report — so error rendering can honor the `--json` flag, which
//! is only known after parsing.

pub mod cli;
mod commands;
pub mod core;
pub mod error;
mod logging;
mod mcp;
mod output;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;
use crate::output::OutputMode;

/// Parse arguments, initialize logging, execute the command, and report.
///
/// Returns the process exit code: success, or the failed command's
/// [`error::Error::exit_code`] after rendering the error to stderr.
#[must_use]
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    logging::init(cli.verbose, cli.quiet);
    let mode = OutputMode::from_cli(&cli, cli.command.name());
    let root = cli.bundle_dir();

    match commands::dispatch(cli.command, &root, mode) {
        Ok(code) => code,
        Err(err) => {
            mode.report_error(&err);
            ExitCode::from(err.exit_code())
        }
    }
}
