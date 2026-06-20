# CLAUDE.md

Guidance for working in this repository. These rules are enforced by lints and
CI — follow them, don't fight them.

## What this is

`okf` is a cross-platform (Linux, macOS, Windows) command-line tool written in
Rust. It targets the current stable Rust toolchain (edition 2024).

## Non-negotiable rules

These are enforced by `[lints.clippy]` in `Cargo.toml` and fail the build:

- **Never `unwrap()`** — denied (`clippy::unwrap_used`).
- **Never `expect()`** — denied (`clippy::expect_used`).
- **Never `panic!`** (or `todo!`, `unimplemented!`, `unreachable!`) — denied.
- **No `process::exit`** — return an error or `ExitCode` from `main`.
- **No raw indexing** (`slice[i]`) that can panic — use `.get(i)`.
- **No `unsafe`** — `unsafe_code` is `forbid`-en at the crate root.

The only exception: `#[cfg(test)]` modules may allow `unwrap_used`/`expect_used`
locally, because in a test a panic *is* the failed assertion. Add
`#![allow(clippy::unwrap_used, clippy::expect_used)]` at the top of the test
module, never crate-wide.

## Error handling

- All fallible code returns `crate::error::Result<T>` (alias for
  `Result<T, crate::error::Error>`).
- `Error` (in `src/error.rs`) is a `thiserror` enum. Each variant's `#[error("…")]`
  message must be **actionable and user-facing** — name the file, the value, the
  thing that went wrong. The user sees this string; assume they can't read code.
- **Every error is a real, named variant.** No `anyhow`, no catch-all/`Box<dyn Error>`
  variant. When a new failure mode appears, add a dedicated variant for it.
- Carry the underlying cause with `#[source]` (and `#[from]` for clean `?`
  conversion) so the cause chain prints. Propagate with `?`; convert foreign
  errors into a typed variant via `.map_err(...)` or a `#[from]` impl.
- Exit codes live in `Error::exit_code()`. `main` prints the error plus its cause
  chain to stderr and returns the code.

## Project layout

```
src/
  main.rs          Thin binary: run() -> report errors -> ExitCode.
  lib.rs           run(): parse args, init logging, dispatch.
  cli.rs           clap derive structs — the typed model of user intent.
  error.rs         Error enum + Result alias + exit codes.
  logging.rs       tracing subscriber setup from -v/-q flags and RUST_LOG.
  commands/
    mod.rs         dispatch(Command) -> Result<()>.
    <name>.rs      One module per subcommand, exposing `run(args) -> Result<()>`.
```

Logic lives in the library so it can be unit-tested; `main.rs` stays thin.

## Adding a subcommand

1. Add a variant to `cli::Command` with clap-documented args (`///` doc comments
   become `--help` text).
2. Create `src/commands/<name>.rs` with `pub fn run(...) -> Result<()>` and unit
   tests.
3. Wire it into `commands::dispatch`.

## Verify before done

```
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

All four must pass. `-D warnings` turns the lint groups into hard failures, so a
clean clippy run is the bar.
