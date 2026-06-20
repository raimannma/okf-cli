//! Logging setup.
//!
//! Verbosity from CLI flags maps onto a tracing filter. `RUST_LOG`, when set,
//! takes precedence so users can override the level entirely.

use tracing_subscriber::EnvFilter;

/// Initialize the global tracing subscriber.
///
/// `verbose` is the repeat count of `-v`; `quiet` suppresses everything below
/// errors. If a subscriber is already set this is a no-op, so it is safe to call
/// once at startup.
pub(crate) fn init(verbose: u8, quiet: bool) {
    let level = if quiet {
        "error"
    } else {
        match verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    // `try_init` returns Err only if a subscriber is already installed, which is
    // not an error condition for us — ignore it deliberately.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
