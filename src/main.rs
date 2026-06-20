//! Binary entry point.
//!
//! Kept intentionally thin: all logic — including error reporting and exit-code
//! mapping — lives in [`okf::run`]. Returning [`ExitCode`] (rather than
//! `process::exit`) means destructors still run.

use std::process::ExitCode;

fn main() -> ExitCode {
    okf::run()
}
