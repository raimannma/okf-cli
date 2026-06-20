# okf

An **agent-oriented** command-line tool for working with [Open Knowledge Format
(OKF)](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf)
bundles — directories of plain Markdown files (with YAML frontmatter) that
represent organizational knowledge as a cross-linked graph of "concepts".

Where the existing OKF tooling targets humans (`validate`, `info`,
`graph --dot`), `okf` is built for the way an LLM agent actually works:
token-efficient retrieval, structured output, navigable graph queries, and safe,
reversible mutation. The same core library powers both the CLI and a built-in
[MCP](https://modelcontextprotocol.io) server, so an agent can drive it with no
per-framework glue.

Cross-platform (Linux, macOS, Windows), written in Rust.

## When to use it

Reach for `okf` whenever an agent (or you) needs to **read from or write to a
local OKF bundle** without slurping the whole thing into a context window:

- **Survey, then drill down.** `okf list` and `okf search` are cheap; `okf get`
  fetches a single concept — or just one section of it — so "what columns does
  `orders` have?" doesn't cost the entire document.
- **Navigate the graph.** `okf neighbors` answers "what links to this, and what
  does it link to?"; `okf context` assembles a self-contained blob of a concept
  plus everything it cites, ready to inject into a prompt.
- **Mutate safely.** `set`, `patch`, `link`, `move`, `remove`, … all support
  `--dry-run` (a unified-diff preview), gate writes on OKF conformance, and keep
  the link graph and `index.md` listings consistent automatically.
- **Wire it into an agent.** `okf serve --mcp` exposes every operation as an MCP
  tool; `okf skills install` drops a self-describing skill file into an agent's
  skills directory.

If you just need to *author* a bundle by hand, a text editor is fine — `okf`
earns its keep when something is querying or editing the bundle programmatically.

## Install

Requires a current stable Rust toolchain (edition 2024). The crate is published
as `okf-cli`; the installed binary is named `okf`.

```sh
cargo install okf-cli
```

To include the optional, token-efficient `--format toon` output for LLM prompts:

```sh
cargo install okf-cli --features toon
```

### From source

```sh
git clone https://github.com/raimannma12/okf-cli
cd okf-cli
cargo install --path .          # or: cargo build --release
```

`cargo build --release` produces the binary at `target/release/okf`
(`okf.exe` on Windows).

## Quick start

Point `okf` at a bundle with `-C <dir>` (or the `OKF_DIR` environment variable);
it defaults to the current directory.

```sh
# Survey the bundle, cheaply (IDs + frontmatter, never bodies)
okf -C ./sales list
okf -C ./sales list --type "BigQuery Table" --tag revenue

# Find something
okf -C ./sales search "weekly active users"

# Read one concept, or just one section of it
okf -C ./sales get tables/orders
okf -C ./sales get tables/orders --section "# Schema"

# Walk the graph
okf -C ./sales neighbors tables/orders --depth 2
okf -C ./sales context metrics/weekly_active_users --depth 2

# Safe mutation — preview first
okf -C ./sales set tables/refunds --type "BigQuery Table" \
    --title "Refunds" --body "# Schema..." --dry-run
okf -C ./sales link tables/orders tables/customers --dry-run
```

## Commands

| Command | What it does |
|---|---|
| `list` | Enumerate concepts with `--type` / `--tag` / `--path-prefix` / `--modified-since` filters (IDs + frontmatter only). |
| `search <query>` | BM25 keyword search over bodies and frontmatter; ranked IDs with snippets. |
| `get <ids…>` | Fetch concepts, with `--frontmatter-only`, `--body-only`, or `--section`. |
| `neighbors <id>` | Outbound links and backlinks ("cited by"), with `--depth N`. |
| `context <id>` | Assemble a concept plus everything it links to into one injectable blob. |
| `resolve <link>` | Resolve a markdown link to its canonical concept ID and whether it exists. |
| `set <id>` | Create or update a concept (frontmatter flags + body); validates before writing. |
| `patch <id> --section` | Replace or append a single section in place. |
| `link` / `unlink` | Add or remove a cross-link as a correct bundle-relative markdown link. |
| `move <old> <new>` | Rename a concept, rewriting every inbound link across the bundle. |
| `remove <ids…>` | Delete concepts and scrub every link to them. |
| `log --append` | Append a dated entry to the reserved `log.md` change history. |
| `index` | (Re)generate the reserved `index.md` listings; `--check` is a CI drift gate. |
| `check` | Report broken links and unparseable files (read-only; exits non-zero if any). |
| `fmt <ids…>` | Normalize concepts to canonical on-disk form (`-w` to write). |
| `serve --mcp` | Run as an MCP server over stdio, exposing every operation as a tool. |
| `skills install` | Generate and install a self-describing agent skill (`--agent claude\|codex`). |

Every mutating command supports `--dry-run` (preview as a unified diff) and
gates the write on OKF conformance — the result must carry a non-empty `type`
(SPEC §9) — unless `--force` is given. Run `okf <command> --help` for full flag
documentation.

## Output contract

`okf` is JSON-first. Use `--format json` (or `-o json`) for a single,
machine-readable envelope per invocation:

```json
{ "command": "list", "ok": true, "data": { … }, "warnings": [] }
```

Failures use the same shape with `"ok": false` and an `"error"` field. The
default `text` format is self-describing and human-readable. With the `toon`
feature, `--format toon` emits the same envelope in a token-efficient encoding
for LLM prompts.

Exit codes let scripts branch on failure class:

| Code | Meaning |
|---|---|
| `0` | Success. |
| `1` | I/O or internal error (read/write failure, parse error, …). |
| `2` | Invalid input or a refused write (bad argument, concept already exists, …). |
| `3` | Concept or section not found. |
| `4` | Result is not OKF-conformant (no non-empty `type`). |
| `5` | `check` / `index --check` found problems or drift. |

## Logging

```sh
okf -v list      # more logging; -vv, -vvv for more
okf --quiet list # errors only
```

`RUST_LOG` (e.g. `RUST_LOG=debug`) overrides the `-v`/`-q` flags. Logs and
diagnostics go to stderr; results go to stdout.

## Development

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

All four must pass. See [CLAUDE.md](CLAUDE.md) for the project's coding rules —
notably: no `unwrap`, no `expect`, no `panic`; everything returns a typed
`Result` with user-facing error messages, enforced by clippy lints in
`Cargo.toml`. [ROADMAP.md](ROADMAP.md) tracks what's built and what's planned.

## License

Licensed under the [MIT License](LICENSE).
