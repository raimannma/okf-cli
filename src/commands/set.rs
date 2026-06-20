//! `set` subcommand: create or update a concept, safely.
//!
//! The first mutating command, so it establishes the Phase 2 contract: build the
//! new document, gate it on OKF conformance (non-empty `type`, SPEC §9), and only
//! then write — with `--dry-run` previewing the change as a unified diff and
//! `--force` overriding the gate. Frontmatter merges onto the concept's current
//! frontmatter so producer-defined keys survive a round-trip (SPEC §4.1), and an
//! unchanged write is a no-op so re-running `set` is idempotent.

use std::path::Path;

use crate::commands::index::{self, IndexChange};
use crate::commands::mutate::{
    broken_link_warnings, concept_path, conformance_note, normalize_body, parse_concept_id,
    read_existing, read_stdin_if_piped, render_document, require_conformant_fm, unified_diff,
    write_concept,
};
use crate::core::index::ConceptSummary;
use crate::core::{Bundle, Concept, Frontmatter};
use crate::error::{Error, Result};
use crate::output::OutputMode;

/// The frontmatter and body edits requested on the command line.
#[derive(Debug, Default)]
pub(crate) struct SetArgs {
    pub type_: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// A JSON/YAML object of arbitrary frontmatter keys to merge in.
    pub frontmatter: Option<String>,
    /// The body supplied inline via `--body`; `None` falls back to stdin.
    pub body: Option<String>,
    pub dry_run: bool,
    pub force: bool,
    /// Regenerate the affected `index.md` listings after writing (default on).
    pub reindex: bool,
}

/// Build, validate, and write (or preview) the concept `concept_id` under `root`.
///
/// # Errors
///
/// Returns [`Error::ReservedConcept`] for an `index`/`log` target,
/// [`Error::InvalidInput`] for a malformed id, [`Error::InvalidFrontmatterArg`]
/// for a bad `--frontmatter`, [`Error::NotConformant`] when the result has no
/// non-empty `type` and `--force` was not given, [`Error::ReadStdin`] /
/// [`Error::WriteFile`] on I/O failure, or [`Error::RenderFrontmatter`] if the
/// frontmatter cannot be serialized.
pub(crate) fn run(root: &Path, concept_id: &str, args: &SetArgs, mode: OutputMode) -> Result<()> {
    let id = parse_concept_id(concept_id)?;
    let path = concept_path(root, &id);
    let path_display = format!("{id}.md");

    // Read the prior file verbatim so the diff and the created/updated decision
    // reflect exactly what is on disk.
    let prior = read_existing(&path, &id)?;
    let created = prior.is_none();

    // An existing, parseable concept seeds the frontmatter merge and the
    // kept-body fallback; an unparseable file contributes neither (the write
    // replaces it, which the dry-run diff makes visible before it happens).
    let existing = prior
        .as_deref()
        .and_then(|content| Concept::parse(id.clone(), content).ok());

    let frontmatter = build_frontmatter(existing.as_ref().map(|c| &c.frontmatter), args)?;
    let conformant = require_conformant_fm(&frontmatter, &id, args.force, "write")?;

    let body = resolve_body(args, existing.as_ref().map(|c| c.body.as_str()))?;
    let content = render_document(&id, &frontmatter, &body)?;

    let prior_str = prior.as_deref().unwrap_or("");
    let changed = prior_str != content;
    let diff = unified_diff(prior_str, &content, &path_display);

    // Idempotent: an unchanged write touches nothing. A dry run never writes.
    let written = !args.dry_run && (created || changed);
    if written {
        write_concept(&path, &id, &content)?;
    }

    let action = if created {
        "created"
    } else if changed {
        "updated"
    } else {
        "unchanged"
    };

    // Keep the directory listings in sync with the concept just written. The
    // post-state is the bundle with this concept's id mapped to its new
    // frontmatter, so the preview is correct even under --dry-run.
    let indexes = if args.reindex {
        let bundle = Bundle::load(root)?;
        let summary = ConceptSummary::from_frontmatter(id.clone(), &frontmatter);
        index::sync(
            root,
            &bundle,
            std::slice::from_ref(&id),
            vec![summary],
            args.dry_run,
        )?
    } else {
        Vec::new()
    };

    let output = SetOutput {
        concept: id.to_string(),
        path: path_display,
        action,
        written,
        dry_run: args.dry_run,
        conformant,
        diff,
        indexes,
    };
    let warnings = broken_link_warnings(root, &id, &content);
    mode.emit(&output, &output.render_human(), &warnings);
    Ok(())
}

/// Compose the final frontmatter: start from the existing block (or empty),
/// overlay the `--frontmatter` object, then apply the typed flags last so they
/// win on conflict and land in OKF's recommended key order for a new concept.
fn build_frontmatter(existing: Option<&Frontmatter>, args: &SetArgs) -> Result<Frontmatter> {
    let mut fm = existing.cloned().unwrap_or_default();
    if let Some(raw) = &args.frontmatter {
        let overlay =
            Frontmatter::parse(raw).map_err(|source| Error::InvalidFrontmatterArg { source })?;
        fm.merge(&overlay);
    }
    if let Some(type_) = &args.type_ {
        fm.set_str("type", type_);
    }
    if let Some(title) = &args.title {
        fm.set_str("title", title);
    }
    if let Some(description) = &args.description {
        fm.set_str("description", description);
    }
    if !args.tags.is_empty() {
        fm.set_tags(&args.tags);
    }
    Ok(fm)
}

/// Resolve the body to write: `--body`, else piped stdin, else the existing
/// body, else empty. A non-empty body is normalized to end in exactly one
/// newline so written files stay POSIX-clean.
///
/// Empty piped input counts as "no body given" (so an update keeps its body and
/// a bare pipeline does not silently blank a concept); an empty body is set only
/// by an explicit `--body ""`.
fn resolve_body(args: &SetArgs, existing: Option<&str>) -> Result<String> {
    let raw = match &args.body {
        Some(body) => Some(body.clone()),
        None => read_stdin_if_piped()?.filter(|s| !s.is_empty()),
    };
    let Some(body) = raw else {
        return Ok(existing.unwrap_or("").to_owned());
    };
    Ok(normalize_body(&body))
}

/// The stable JSON contract for `okf set`.
#[derive(Debug, serde::Serialize)]
struct SetOutput {
    /// The concept ID that was written.
    concept: String,
    /// Its bundle-relative file path.
    path: String,
    /// What happened: `created`, `updated`, or `unchanged`. Under `--dry-run`
    /// this is what *would* happen (see `written`).
    action: &'static str,
    /// Whether the file was actually written (false for a dry run or a no-op).
    written: bool,
    /// Whether this invocation was a preview only.
    dry_run: bool,
    /// Whether the result satisfies OKF §9 (a non-empty `type`). Only ever false
    /// when `--force` let a non-conformant write through.
    conformant: bool,
    /// A unified diff of the change; empty when nothing changed.
    #[serde(skip_serializing_if = "str::is_empty")]
    diff: String,
    /// The `index.md` listings regenerated to keep up with the change; empty when
    /// reindexing is off or nothing drifted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    indexes: Vec<IndexChange>,
}

impl SetOutput {
    fn render_human(&self) -> String {
        let mut out = if self.dry_run {
            match self.action {
                "unchanged" => format!("[dry run] {} is already up to date\n", self.path),
                verb => format!("[dry run] would {verb} {}\n{}", self.path, self.diff),
            }
        } else {
            let note = conformance_note(self.conformant);
            format!("{} {}{note}\n", self.action, self.path)
        };
        index::append_human_summary(&mut out, &self.indexes, self.dry_run);
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn args() -> SetArgs {
        SetArgs::default()
    }

    #[test]
    fn typed_flags_win_over_frontmatter_overlay() {
        let mut a = args();
        a.frontmatter = Some(r#"{"type": "Old", "owner": "x"}"#.to_owned());
        a.type_ = Some("Table".to_owned());
        let fm = build_frontmatter(None, &a).unwrap();
        assert_eq!(fm.type_(), Some("Table"));
        assert_eq!(fm.get_str("owner"), Some("x"));
    }

    #[test]
    fn merge_preserves_unknown_existing_keys() {
        let existing = Frontmatter::parse("type: Table\nowner: data-team").unwrap();
        let mut a = args();
        a.title = Some("Orders".to_owned());
        let fm = build_frontmatter(Some(&existing), &a).unwrap();
        // The producer-defined `owner` survives; `title` is added.
        assert_eq!(fm.get_str("owner"), Some("data-team"));
        assert_eq!(fm.title(), Some("Orders"));
    }

    #[test]
    fn invalid_frontmatter_arg_is_a_typed_error() {
        let mut a = args();
        a.frontmatter = Some("- not\n- a\n- map".to_owned());
        assert!(matches!(
            build_frontmatter(None, &a),
            Err(Error::InvalidFrontmatterArg { .. })
        ));
    }

    #[test]
    fn creates_a_new_concept_and_writes_conformant_file() {
        let dir = TempDir::new().unwrap();
        let mut a = args();
        a.type_ = Some("Table".to_owned());
        a.body = Some("# Schema\n\n- id".to_owned());
        let mode = OutputMode::test(crate::cli::Format::Text, false, "set");

        run(dir.path(), "tables/orders", &a, mode).unwrap();

        let written = fs::read_to_string(dir.path().join("tables/orders.md")).unwrap();
        assert_eq!(written, "---\ntype: Table\n---\n# Schema\n\n- id\n");
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let mut a = args();
        a.type_ = Some("Table".to_owned());
        a.dry_run = true;
        let mode = OutputMode::test(crate::cli::Format::Text, false, "set");

        run(dir.path(), "tables/orders", &a, mode).unwrap();
        assert!(!dir.path().join("tables/orders.md").exists());
    }

    #[test]
    fn non_conformant_write_is_gated_unless_forced() {
        let dir = TempDir::new().unwrap();
        let mode = OutputMode::test(crate::cli::Format::Text, false, "set");

        // No `type`: rejected.
        let mut a = args();
        a.title = Some("Orphan".to_owned());
        let err = run(dir.path(), "notes/x", &a, mode).unwrap_err();
        assert!(matches!(err, Error::NotConformant { .. }));
        assert!(!dir.path().join("notes/x.md").exists());

        // `--force` lets it through.
        let mut a = args();
        a.title = Some("Orphan".to_owned());
        a.force = true;
        run(dir.path(), "notes/x", &a, mode).unwrap();
        assert!(dir.path().join("notes/x.md").exists());
    }

    #[test]
    fn updating_keeps_existing_body_and_unknown_keys() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("tables")).unwrap();
        fs::write(
            dir.path().join("tables/orders.md"),
            "---\ntype: Table\nowner: data-team\n---\n# Schema\n\n- id\n",
        )
        .unwrap();
        let mode = OutputMode::test(crate::cli::Format::Text, false, "set");

        // Set only the title; no body provided.
        let mut a = args();
        a.title = Some("Customer Orders".to_owned());
        run(dir.path(), "tables/orders", &a, mode).unwrap();

        let written = fs::read_to_string(dir.path().join("tables/orders.md")).unwrap();
        assert_eq!(
            written,
            "---\ntype: Table\nowner: data-team\ntitle: Customer Orders\n---\n# Schema\n\n- id\n"
        );
    }

    #[test]
    fn reindex_reflects_updated_frontmatter_in_the_listing() {
        let dir = TempDir::new().unwrap();
        let mode = OutputMode::test(crate::cli::Format::Text, false, "set");

        let mut a = args();
        a.type_ = Some("Table".to_owned());
        a.title = Some("Orders".to_owned());
        a.description = Some("Old blurb.".to_owned());
        a.reindex = true;
        run(dir.path(), "tables/orders", &a, mode).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("tables/index.md")).unwrap(),
            "# Table\n\n* [Orders](orders.md) - Old blurb.\n"
        );

        // Updating the description rewrites the index entry to match.
        let mut a = args();
        a.description = Some("New blurb.".to_owned());
        a.reindex = true;
        run(dir.path(), "tables/orders", &a, mode).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("tables/index.md")).unwrap(),
            "# Table\n\n* [Orders](orders.md) - New blurb.\n"
        );
    }

    #[test]
    fn rewriting_identical_content_is_a_noop() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("tables")).unwrap();
        let path = dir.path().join("tables/orders.md");
        fs::write(&path, "---\ntype: Table\n---\n# Schema\n").unwrap();
        let before = fs::metadata(&path).unwrap().modified().unwrap();

        let mut a = args();
        a.type_ = Some("Table".to_owned());
        a.body = Some("# Schema".to_owned());
        let mode = OutputMode::test(crate::cli::Format::Text, false, "set");
        run(dir.path(), "tables/orders", &a, mode).unwrap();

        // Unchanged write must not rewrite the file (mtime preserved).
        let after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after);
    }
}
