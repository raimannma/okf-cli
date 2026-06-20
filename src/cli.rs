//! Command-line interface definition.
//!
//! The parsed [`Cli`] is the single source of truth for user intent; everything
//! downstream operates on these typed values rather than raw strings.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// How a command renders its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Format {
    /// Self-describing, compact, human-readable text (the default).
    #[default]
    Text,
    /// A single machine-readable JSON envelope: `{command, ok, data, warnings}`.
    Json,
    /// The same envelope encoded as TOON — a token-efficient JSON alternative
    /// for LLM prompts. Requires the `toon` build feature.
    #[cfg(feature = "toon")]
    Toon,
}

/// Top-level CLI.
#[derive(Debug, Parser)]
#[command(name = "okf", version, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    /// Output format: `text` (default) or `json`.
    ///
    /// `json` emits one envelope object per invocation — `{command, ok, data,
    /// warnings}` on success, `{command, ok: false, error, warnings}` on failure
    /// — so a harness can parse every result the same way.
    #[arg(
        short = 'o',
        long,
        global = true,
        value_enum,
        default_value_t = Format::Text,
        value_name = "FORMAT"
    )]
    pub format: Format,

    /// Deprecated alias for `--format json`.
    #[arg(long, global = true, hide = true)]
    pub json: bool,

    /// Increase logging verbosity (repeat for more: -v, -vv, -vvv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Silence all output except errors.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Run as if okf was started in <PATH> instead of the current working
    /// directory.
    ///
    /// When given multiple times, each subsequent non-absolute `-C <PATH>` is
    /// interpreted relative to the preceding one. An empty path (`-C ""`) leaves
    /// the working directory unchanged. Falls back to the `OKF_DIR` environment
    /// variable when no `-C` is given on the command line.
    #[arg(short = 'C', global = true, value_name = "PATH", env = "OKF_DIR")]
    pub directory: Vec<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// The output format the user asked for, honoring the deprecated `--json`
    /// alias (which forces JSON regardless of `--format`).
    #[must_use]
    pub fn effective_format(&self) -> Format {
        if self.json { Format::Json } else { self.format }
    }

    /// The bundle root directory after applying the `-C` chain to the current
    /// working directory (git semantics: absolute paths reset, relative paths
    /// compose, empty paths are no-ops).
    #[must_use]
    pub fn bundle_dir(&self) -> PathBuf {
        let mut dir = PathBuf::from(".");
        for path in &self.directory {
            if path.as_os_str().is_empty() {
                continue;
            }
            // `join` resets to `path` when it is absolute, and composes onto the
            // running directory when it is relative — exactly the `-C` rule.
            dir = dir.join(path);
        }
        dir
    }
}

/// The available subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the concepts in a bundle (IDs and frontmatter, never bodies).
    List {
        /// Only list concepts whose ID starts with this path prefix (e.g. `tables/`).
        #[arg(long, value_name = "PREFIX")]
        path_prefix: Option<String>,

        /// Only list concepts whose `type` is one of these (repeatable; matches any).
        #[arg(long = "type", value_name = "TYPE")]
        type_: Vec<String>,

        /// Only list concepts carrying every one of these tags (repeatable; matches all).
        #[arg(long = "tag", value_name = "TAG")]
        tag: Vec<String>,

        /// Only list concepts modified at or after this time.
        ///
        /// Accepts an RFC 3339 datetime (`2026-01-15T10:30:00Z`) or a bare
        /// `YYYY-MM-DD` date (treated as midnight UTC). A concept's modified time
        /// is its `timestamp` frontmatter field, falling back to the file's
        /// last-modified time when that field is absent.
        #[arg(long, value_name = "WHEN")]
        modified_since: Option<String>,
    },

    /// Search concept bodies and frontmatter, returning ranked IDs with snippets.
    Search {
        /// The query. Split into whole-word terms (case-insensitive); a concept
        /// matches if it contains any term, ranked by BM25 relevance.
        ///
        /// Multiple words may be passed unquoted (`search suggested edits`) or as
        /// a single quoted argument (`search 'suggested edits'`) — equivalently.
        #[arg(value_name = "QUERY", required = true, num_args = 1..)]
        query: Vec<String>,

        /// Return at most this many top-ranked hits (0 means no limit).
        #[arg(long, value_name = "N", default_value_t = 20)]
        limit: usize,

        /// Drop hits whose BM25 score is below this floor (0 keeps every match).
        ///
        /// Filters out weak matches — typically hits on a term so common it
        /// appears in nearly every concept and so carries almost no signal.
        #[arg(long, value_name = "SCORE", default_value_t = 0.5)]
        min_score: f64,
    },

    /// Fetch one or more concepts, optionally only part of each.
    Get {
        /// The concept IDs — each a bundle-relative path without `.md` (e.g. `tables/orders`).
        #[arg(value_name = "CONCEPT_ID", required = true, num_args = 1..)]
        concept_ids: Vec<String>,

        /// Return only the frontmatter, not the body.
        #[arg(long, conflicts_with_all = ["body_only", "section"])]
        frontmatter_only: bool,

        /// Return only the markdown body, not the frontmatter.
        #[arg(long, conflicts_with = "section")]
        body_only: bool,

        /// Return only the content under this heading (e.g. `--section "# Schema"`).
        ///
        /// Matches with or without leading `#`s; when `#`s are given the heading
        /// level must match too. Spans the heading and everything beneath it up to
        /// the next heading of the same or higher level.
        #[arg(long, value_name = "HEADING")]
        section: Option<String>,
    },

    /// Show a concept's graph neighbors: outbound links and backlinks ("cited by").
    ///
    /// Returns only IDs (with whether each exists and how far it sits), never
    /// bodies — the targeted graph query, not a whole-graph dump.
    #[command(visible_alias = "neighbours")]
    Neighbors {
        /// The concept ID — a bundle-relative path without `.md` (e.g. `tables/orders`).
        #[arg(value_name = "CONCEPT_ID")]
        concept_id: String,

        /// Follow the link graph this many hops in each direction (default 1).
        ///
        /// Depth 1 is the concept's direct neighbors. Higher depths add the
        /// transitively reachable concepts, each tagged with its shortest
        /// distance from the concept. Outbound and inbound are traversed
        /// independently and reported separately.
        #[arg(long, value_name = "N", default_value_t = 1)]
        depth: usize,
    },

    /// Assemble a self-contained context slice for a concept, ready to inject.
    ///
    /// Packs the concept itself plus every concept it links to — out to `--depth`
    /// hops along outbound links — into one blob: full documents, the root first
    /// then nearest first. In the default text format this is a markdown blob; with
    /// `--format json` it is the structured envelope. Referenced concepts the bundle
    /// never loaded (broken links) can't be inlined and are listed separately.
    Context {
        /// The concept ID — a bundle-relative path without `.md` (e.g. `metrics/wau`).
        #[arg(value_name = "CONCEPT_ID")]
        concept_id: String,

        /// Follow outbound links this many hops from the concept (default 1).
        ///
        /// Depth 1 is the concept plus what it directly links to. Higher depths
        /// pull in the transitively reachable concepts too, each tagged with its
        /// shortest distance from the root.
        #[arg(long, value_name = "N", default_value_t = 1)]
        depth: usize,
    },

    /// Create or update a concept: set its frontmatter and body, then write it.
    ///
    /// Frontmatter is built by merging onto the concept's current frontmatter
    /// (so unknown keys are preserved): first the `--frontmatter` JSON overlay,
    /// then the typed flags (`--type`, `--title`, …), which win on conflict. The
    /// body comes from `--body`, else piped standard input, else — when updating
    /// an existing concept — its current body is kept.
    ///
    /// The write is gated on OKF conformance: the result must carry a non-empty
    /// `type` (SPEC §9) unless `--force` is given. `--dry-run` previews the change
    /// as a unified diff and writes nothing. Writing identical content is a no-op.
    Set {
        /// The concept ID to write — a bundle-relative path without `.md` (e.g. `tables/orders`).
        #[arg(value_name = "CONCEPT_ID")]
        concept_id: String,

        /// Set the `type` frontmatter field (the one field OKF requires).
        #[arg(long = "type", value_name = "TYPE")]
        type_: Option<String>,

        /// Set the `title` frontmatter field.
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,

        /// Set the `description` frontmatter field.
        #[arg(long, value_name = "TEXT")]
        description: Option<String>,

        /// Set the `tags` frontmatter list (repeatable; replaces any existing tags).
        #[arg(long = "tag", value_name = "TAG")]
        tag: Vec<String>,

        /// Merge these frontmatter keys, as a JSON object (e.g. `--frontmatter '{"owner":"x"}'`).
        ///
        /// Applied before the typed flags, so a `--title` still wins over a
        /// `title` given here. Use this for keys without a dedicated flag.
        #[arg(long, value_name = "JSON")]
        frontmatter: Option<String>,

        /// Provide the body inline instead of on standard input.
        #[arg(long, value_name = "MARKDOWN")]
        body: Option<String>,

        /// Preview the change as a unified diff without writing anything.
        #[arg(long)]
        dry_run: bool,

        /// Write even when the result is not conformant (has no non-empty `type`).
        #[arg(long)]
        force: bool,

        /// Do not regenerate the affected `index.md` listings after writing.
        #[arg(long)]
        no_reindex: bool,
    },

    /// Replace or append a single section of an existing concept, in place.
    ///
    /// The surgical counterpart to `set`: it edits just one heading's content and
    /// leaves the rest of the file byte-for-byte untouched. New content comes from
    /// `--content` or, if that is omitted, standard input. By default the named
    /// section's body is replaced; with `--append` the content is added to the end
    /// of that section instead. A section that does not exist is created at the end
    /// of the document.
    ///
    /// You supply the section *body*, not its `#` heading — okf owns the heading
    /// line (and, for a created section, derives it from `--section`, defaulting a
    /// missing `#` to level 1). The concept must already exist (use `set` to create
    /// one). The write is gated on conformance (a non-empty `type`, OKF §9) unless
    /// `--force`; `--dry-run` previews the change as a unified diff and writes
    /// nothing; an edit that changes nothing is a no-op.
    Patch {
        /// The concept ID to edit — a bundle-relative path without `.md` (e.g. `tables/orders`).
        #[arg(value_name = "CONCEPT_ID")]
        concept_id: String,

        /// The section heading to target (e.g. `--section "# Joins"`).
        ///
        /// Matched as in `get --section`: with or without leading `#`s, and when
        /// `#`s are given the heading level must match too.
        #[arg(long, value_name = "HEADING", required = true)]
        section: String,

        /// Provide the new section content inline instead of on standard input.
        ///
        /// This is the section body only — do not repeat the `#` heading. An empty
        /// value (`--content ""`) clears the section's content.
        ///
        /// Leading `-` is allowed (markdown list items are common), so the value is
        /// taken verbatim — `--content "- a\n- b"` needs no escaping.
        #[arg(long, value_name = "MARKDOWN", allow_hyphen_values = true)]
        content: Option<String>,

        /// Append to the section's existing content instead of replacing it.
        #[arg(long)]
        append: bool,

        /// Preview the change as a unified diff without writing anything.
        #[arg(long)]
        dry_run: bool,

        /// Edit even when the concept is not conformant (has no non-empty `type`).
        #[arg(long)]
        force: bool,
    },

    /// Add a cross-link from one concept to another as a markdown link.
    ///
    /// Writes a `- [text](/to.md)` bullet into a section of `<from>` (default
    /// `# Related`, created if absent), using the unambiguous bundle-relative
    /// link form so the edit can't be a malformed path. The link text defaults to
    /// the target's `title`, falling back to its id; override with `--text`.
    ///
    /// Idempotent: if `<from>` already links to `<to>` anywhere in its body, this
    /// does nothing. Linking to a target that does not exist is allowed — broken
    /// links are legal "reference-first" placeholders (SPEC §5) — but warns. The
    /// write is gated on conformance (a non-empty `type`, OKF §9) unless `--force`;
    /// `--dry-run` previews the change as a unified diff and writes nothing.
    Link {
        /// The concept to add the link to — a bundle-relative path without `.md` (e.g. `tables/orders`).
        #[arg(value_name = "FROM")]
        from: String,

        /// The concept to link to — a bundle-relative path without `.md` (e.g. `tables/customers`).
        #[arg(value_name = "TO")]
        to: String,

        /// The section to place the link under (e.g. `--section "# Metrics"`).
        ///
        /// Matched as in `get --section`. Created at the end of the document when
        /// absent; an existing section gains one more bullet.
        #[arg(long, value_name = "HEADING", default_value = "# Related")]
        section: String,

        /// The link text to use instead of the target's title (or id).
        #[arg(long, value_name = "TEXT")]
        text: Option<String>,

        /// Preview the change as a unified diff without writing anything.
        #[arg(long)]
        dry_run: bool,

        /// Edit even when `<from>` is not conformant (has no non-empty `type`).
        #[arg(long)]
        force: bool,
    },

    /// Remove a cross-link from one concept to another.
    ///
    /// Drops every list-item bullet in `<from>` whose link resolves to `<to>` —
    /// the bullet form `link` writes and that bundles use for "related" lists. A
    /// link buried in prose is left intact (and reported as a warning) rather than
    /// rewriting a sentence. No-op when `<from>` has no such bullet. The write is
    /// gated on conformance unless `--force`; `--dry-run` previews the diff.
    Unlink {
        /// The concept to remove the link from — a bundle-relative path without `.md`.
        #[arg(value_name = "FROM")]
        from: String,

        /// The link target to remove — a bundle-relative path without `.md`.
        #[arg(value_name = "TO")]
        to: String,

        /// Preview the change as a unified diff without writing anything.
        #[arg(long)]
        dry_run: bool,

        /// Edit even when `<from>` is not conformant (has no non-empty `type`).
        #[arg(long)]
        force: bool,
    },

    /// Rename or move a concept, rewriting every inbound link across the bundle.
    ///
    /// Renames the file from `<OLD_ID>` to `<NEW_ID>` and rewrites every concept
    /// that links to it so the link points at the new id, in the canonical
    /// bundle-relative form (`/new-id.md`). When the move changes the concept's
    /// directory, its own document-relative outbound links (`../x.md`) are
    /// re-anchored too, so they keep resolving to the same targets.
    ///
    /// Never overwrites: a `<NEW_ID>` that already exists is an error. The move is
    /// gated on conformance of the moved concept (a non-empty `type`, OKF §9)
    /// unless `--force`; `--dry-run` previews every file change as a unified diff
    /// and writes nothing.
    Move {
        /// The concept to move — a bundle-relative path without `.md` (e.g. `tables/orders`).
        #[arg(value_name = "OLD_ID")]
        old_id: String,

        /// The concept's new id — a bundle-relative path without `.md` (e.g. `facts/orders`).
        #[arg(value_name = "NEW_ID")]
        new_id: String,

        /// Preview every file change as a unified diff without writing anything.
        #[arg(long)]
        dry_run: bool,

        /// Move even when the concept is not conformant (has no non-empty `type`).
        #[arg(long)]
        force: bool,

        /// Do not regenerate the affected `index.md` listings after moving.
        #[arg(long)]
        no_reindex: bool,
    },

    /// Delete one or more concepts and scrub every link to them across the bundle.
    ///
    /// Removes each concept's file and drops every list-item bullet that links to
    /// a deleted concept (the bullet form `link` writes). A link buried in prose
    /// is left as a now-broken reference — legal under SPEC §5 — and reported as a
    /// warning. By default a concept id that does not exist is an error and nothing
    /// is deleted; `--force` skips missing ids instead (like `rm -f`). `--dry-run`
    /// previews every change as a unified diff and deletes nothing.
    Remove {
        /// The concept IDs to delete — each a bundle-relative path without `.md` (e.g. `tables/orders`).
        #[arg(value_name = "CONCEPT_ID", required = true, num_args = 1..)]
        concept_ids: Vec<String>,

        /// Preview every change as a unified diff without deleting anything.
        #[arg(long)]
        dry_run: bool,

        /// Skip concept ids that do not exist instead of erroring (like `rm -f`).
        #[arg(long)]
        force: bool,

        /// Do not regenerate the affected `index.md` listings after removing.
        #[arg(long)]
        no_reindex: bool,
    },

    /// Append a dated entry to the bundle's reserved `log.md` change history.
    ///
    /// `log.md` (SPEC §7) is a chronological, append-only change log: date
    /// headings in ISO 8601 `YYYY-MM-DD`, newest first, with prose entries
    /// beneath each. This adds one entry under today's date (UTC), creating that
    /// date heading at the top of the file when it is the day's first entry, and
    /// creating `log.md` itself when absent.
    ///
    /// The entry text comes from `--entry` or, when that is omitted, standard
    /// input — so an agent can pipe a generated changelog line in. Supply the
    /// prose only; okf owns the date heading. The `**Update**` / `**Creation**`
    /// bold lead-in is an OKF convention you write into the entry yourself, not
    /// something okf adds. `--dry-run` previews the change as a unified diff and
    /// writes nothing. Targets the `log.md` at the bundle root; use `-C <dir>` to
    /// append to a nested scope's log instead.
    Log {
        /// Append the entry to the change log (currently the only operation).
        #[arg(long, required = true)]
        append: bool,

        /// The entry prose to append, instead of reading it from standard input.
        ///
        /// Leading `-` is allowed, so an entry may start with a dash without
        /// escaping. An empty value is rejected — pipe or pass real text.
        #[arg(long, value_name = "TEXT", allow_hyphen_values = true)]
        entry: Option<String>,

        /// Preview the change as a unified diff without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Normalize concepts to their canonical on-disk form (preview, or `-w` to write).
    ///
    /// Parses each concept and re-renders it: the frontmatter is re-serialized
    /// with its key order preserved (SPEC §4.1) and the body is kept verbatim
    /// except for collapsing to a single trailing newline — the same
    /// normalization `set` applies, so an agent can write sloppy and get a
    /// canonical file back. The markdown body is never reflowed.
    ///
    /// Read-only by default: it previews each change as a unified diff and writes
    /// nothing until `-w`. A run over several ids is atomic — every concept is
    /// parsed and gated on conformance (a non-empty `type`, OKF §9, unless
    /// `--force`) first, so one unparseable or non-conformant file means nothing
    /// is written.
    Fmt {
        /// The concept IDs to format — each a bundle-relative path without `.md` (e.g. `tables/orders`).
        #[arg(value_name = "CONCEPT_ID", required = true, num_args = 1..)]
        concept_ids: Vec<String>,

        /// Write the canonical form back to each file (without it, `fmt` only previews).
        #[arg(short = 'w', long)]
        write: bool,

        /// Format even a concept that has no non-empty `type` (OKF §9).
        #[arg(long)]
        force: bool,
    },

    /// Resolve a markdown link to its canonical concept ID and whether it exists.
    ///
    /// Applies OKF's link-resolution rules (SPEC §5) so an agent never has to.
    /// External URLs, `mailto:`, and bare `#fragment` links report no concept.
    Resolve {
        /// The markdown link target to resolve (e.g. `/tables/x.md`, `./other.md`).
        #[arg(value_name = "LINK")]
        link: String,

        /// Resolve a document-relative link as if it appeared in this concept.
        ///
        /// A bundle-relative path without `.md` (e.g. `tables/orders`). Required
        /// to make sense of `./` and `../` links; absolute links (`/…`) ignore
        /// it. Without it, the link is resolved as if written at the bundle root.
        #[arg(long, value_name = "CONCEPT_ID")]
        from: Option<String>,
    },

    /// (Re)generate the reserved `index.md` directory listings across the bundle.
    ///
    /// Each directory's `index.md` (SPEC §6) is derived from the concepts beneath
    /// it: its subdirectories under `# Subdirectories` and its concepts grouped by
    /// `type`, each entry a link plus the concept's `description`. This rewrites
    /// every listing that drifted, removes the listing for any directory that no
    /// longer holds concepts, and leaves up-to-date ones untouched. A bundle-root
    /// `index.md` frontmatter block (SPEC §11) and human-written subdirectory
    /// descriptions are preserved across the regeneration.
    ///
    /// The mutating commands keep indexes in sync automatically, so this is mainly
    /// for a one-shot rebuild or a CI gate. `--check` reports which listings are
    /// out of date and exits non-zero without writing (exit code 5); `--dry-run`
    /// previews the changes as unified diffs and writes nothing.
    Index {
        /// Regenerate every `index.md` (the default action).
        #[arg(long)]
        regenerate: bool,

        /// Report out-of-date listings and exit non-zero (code 5); write nothing.
        #[arg(long, conflicts_with_all = ["regenerate", "dry_run"])]
        check: bool,

        /// Preview the changes as unified diffs without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Check the whole bundle for problems, reporting them without changing anything.
    ///
    /// Loads every concept and reports the issues a bundle accumulates across many
    /// edits: links that resolve to a concept absent from the bundle (broken links
    /// — legal "reference-first" placeholders under SPEC §5, but worth surfacing)
    /// and files that could not be parsed as concepts. Read-only and writes
    /// nothing. Exits non-zero (code 5) when any problem is found, so CI can gate
    /// on a clean bundle; exits 0 when the bundle is clean.
    Check {},

    /// Run okf as a server, exposing its operations to agents over a protocol.
    ///
    /// With `--mcp`, speaks the Model Context Protocol over stdio
    /// (newline-delimited JSON-RPC 2.0): every read, search, and mutate command
    /// becomes an MCP tool over the same core library, so a stock MCP-capable
    /// agent can be pointed at a bundle and run the full read → mutate loop with
    /// no per-framework glue. The bundle root is the one selected by `-C` /
    /// `OKF_DIR` (default: the current directory). Logs go to stderr; stdout
    /// carries only protocol traffic.
    Serve {
        /// Serve the Model Context Protocol over stdio (currently required).
        #[arg(long, required = true)]
        mcp: bool,
    },

    /// Generate and install an agent skill that teaches an agent to drive okf.
    ///
    /// The skill content is derived from this live command model, so it always
    /// matches the real CLI — adding a command or flag here shows up in the next
    /// generated skill with no hand-editing.
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
    },
}

/// Operations on agent skill files.
#[derive(Debug, Subcommand)]
pub enum SkillsCommand {
    /// Generate a skill file describing okf and install it for an agent.
    ///
    /// Writes `<skills-dir>/okf/SKILL.md` — the Agent Skills layout shared by
    /// Claude and Codex (a directory named after the skill, holding `SKILL.md`).
    /// The body lists every command and flag, the `--format`/exit-code output
    /// contract, and worked read → assemble → mutate examples.
    Install {
        /// The target agent, selecting the default install directory.
        ///
        /// `claude` installs under `~/.claude/skills`, `codex` under
        /// `~/.agents/skills`. Both formats follow the Agent Skills standard.
        #[arg(long, value_enum, default_value_t = SkillAgent::Claude, value_name = "AGENT")]
        agent: SkillAgent,

        /// Install into this skills directory instead of the agent's default.
        ///
        /// The skill always lands in `<DIR>/okf/SKILL.md`; the `okf/` directory
        /// is required because the Agent Skills spec ties the directory name to
        /// the skill name. Useful for project-local installs and tests.
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,

        /// Print the generated skill to stdout instead of writing it.
        #[arg(long)]
        print: bool,

        /// Preview where the skill would be written, plus its content; write nothing.
        #[arg(long)]
        dry_run: bool,

        /// Overwrite an existing `SKILL.md` instead of refusing.
        #[arg(long)]
        force: bool,
    },
}

/// The agent a generated skill targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SkillAgent {
    /// Claude / Claude Code (`~/.claude/skills`).
    Claude,
    /// `OpenAI` Codex (`~/.agents/skills`).
    Codex,
}

impl Command {
    /// The command's canonical name, used as the `command` field of the JSON
    /// envelope so a harness can tell which command produced a result.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Command::List { .. } => "list",
            Command::Search { .. } => "search",
            Command::Get { .. } => "get",
            Command::Set { .. } => "set",
            Command::Patch { .. } => "patch",
            Command::Link { .. } => "link",
            Command::Unlink { .. } => "unlink",
            Command::Move { .. } => "move",
            Command::Remove { .. } => "remove",
            Command::Log { .. } => "log",
            Command::Fmt { .. } => "fmt",
            Command::Neighbors { .. } => "neighbors",
            Command::Context { .. } => "context",
            Command::Resolve { .. } => "resolve",
            Command::Index { .. } => "index",
            Command::Check { .. } => "check",
            Command::Serve { .. } => "serve",
            Command::Skills { .. } => "skills",
        }
    }
}
