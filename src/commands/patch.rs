//! `patch` subcommand: edit one section of a concept, in place.
//!
//! The surgical counterpart to `set`: instead of rewriting a whole document, it
//! replaces (or appends to) a single heading's content and splices the rest of the
//! body through untouched. This is the primitive that makes an agent's multi-file
//! edits cheap — "add the `# Joins` section to these eight tables" — without risking
//! the parts it didn't mean to touch. It upholds the same Phase 2 contract as
//! `set`: `--dry-run` previews the change as a unified diff, the write is gated on
//! conformance unless `--force`, and an edit that changes nothing is a no-op.

use std::path::Path;

use crate::commands::mutate::{
    broken_link_warnings, concept_path, conformance_note, load_source, parse_concept_id,
    read_stdin_if_piped, render_document, require_conformant, unified_diff, write_concept,
};
use crate::core::concept::SectionEdit;
use crate::error::{Error, Result};
use crate::output::OutputMode;

/// The section edit requested on the command line.
#[derive(Debug, Default)]
pub(crate) struct PatchArgs {
    /// The heading to target, e.g. `# Joins` (matched as in `get --section`).
    pub section: String,
    /// The new section content, supplied inline via `--content`; `None` falls back
    /// to stdin.
    pub content: Option<String>,
    /// Append to the section's existing content instead of replacing it.
    pub append: bool,
    pub dry_run: bool,
    pub force: bool,
}

/// Patch the `section` of concept `concept_id` under `root` (or preview it).
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] for a malformed id or missing content,
/// [`Error::ReservedConcept`] for an `index`/`log` target,
/// [`Error::ConceptNotFound`] if the concept does not exist (use `set` to create
/// one), [`Error::ParseConcept`] if the existing file cannot be parsed,
/// [`Error::NotConformant`] when the concept has no non-empty `type` and `--force`
/// was not given, or [`Error::ReadStdin`] / [`Error::WriteFile`] /
/// [`Error::RenderFrontmatter`] on I/O or rendering failure.
pub(crate) fn run(root: &Path, concept_id: &str, args: &PatchArgs, mode: OutputMode) -> Result<()> {
    let id = parse_concept_id(concept_id)?;
    let path = concept_path(root, &id);
    let path_display = format!("{id}.md");

    // `patch` edits an existing file; creating a concept is `set`'s job.
    let (prior, concept) = load_source(&path, &id)?;

    let conformant = require_conformant(&concept, args.force)?;

    let content = resolve_content(args)?;
    let (new_body, edit) = concept.with_section(&args.section, &content, args.append);
    let new_doc = render_document(&id, &concept.frontmatter, &new_body)?;

    let changed = prior != new_doc;
    let diff = unified_diff(&prior, &new_doc, &path_display);

    let written = !args.dry_run && changed;
    if written {
        write_concept(&path, &id, &new_doc)?;
    }

    let action = PatchAction::new(edit, changed);

    let output = PatchOutput {
        concept: id.to_string(),
        path: path_display,
        section: args.section.clone(),
        action,
        written,
        dry_run: args.dry_run,
        conformant,
        diff,
    };
    let warnings = broken_link_warnings(root, &id, &new_doc);
    mode.emit(&output, &output.render_human(), &warnings);
    Ok(())
}

/// What `patch` did to the targeted section — the JSON `action` value, which also
/// supplies both the past-tense and infinitive verbs the human rendering needs.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum PatchAction {
    Replaced,
    Appended,
    Created,
    Unchanged,
}

impl PatchAction {
    /// Classify an applied [`SectionEdit`]; an edit that changed nothing is
    /// [`PatchAction::Unchanged`] regardless of which branch ran.
    fn new(edit: SectionEdit, changed: bool) -> Self {
        if !changed {
            return Self::Unchanged;
        }
        match edit {
            SectionEdit::Replaced => Self::Replaced,
            SectionEdit::Appended => Self::Appended,
            SectionEdit::Created => Self::Created,
        }
    }

    /// The past-tense verb for the applied-change rendering ("replaced …").
    fn past(self) -> &'static str {
        match self {
            Self::Replaced => "replaced",
            Self::Appended => "appended",
            Self::Created => "created",
            Self::Unchanged => "unchanged",
        }
    }

    /// The infinitive verb for the dry-run "would …" sentence.
    fn infinitive(self) -> &'static str {
        match self {
            Self::Replaced => "replace",
            Self::Appended => "append to",
            Self::Created => "create",
            Self::Unchanged => "leave unchanged",
        }
    }
}

/// Resolve the section content to write: `--content`, else piped stdin. Unlike
/// `set`'s body, content is the thing being patched in, so it is required — an
/// explicit `--content ""` clears the section, but a bare invocation with no pipe
/// is an error rather than a silent empty write.
fn resolve_content(args: &PatchArgs) -> Result<String> {
    let raw = match &args.content {
        Some(content) => Some(content.clone()),
        None => read_stdin_if_piped()?.filter(|s| !s.is_empty()),
    };
    raw.ok_or_else(|| {
        Error::InvalidInput(
            "no section content given; pass --content '…' or pipe it on standard input".to_owned(),
        )
    })
}

/// The stable JSON contract for `okf patch`.
#[derive(Debug, serde::Serialize)]
struct PatchOutput {
    /// The concept ID that was edited.
    concept: String,
    /// Its bundle-relative file path.
    path: String,
    /// The section heading that was targeted, as given on the command line.
    section: String,
    /// What happened: `replaced`, `appended`, `created`, or `unchanged`. Under
    /// `--dry-run` this is what *would* happen (see `written`).
    action: PatchAction,
    /// Whether the file was actually written (false for a dry run or a no-op).
    written: bool,
    /// Whether this invocation was a preview only.
    dry_run: bool,
    /// Whether the concept satisfies OKF §9 (a non-empty `type`). Only ever false
    /// when `--force` let an edit through on a non-conformant concept.
    conformant: bool,
    /// A unified diff of the change; empty when nothing changed.
    #[serde(skip_serializing_if = "str::is_empty")]
    diff: String,
}

impl PatchOutput {
    fn render_human(&self) -> String {
        if self.dry_run {
            if matches!(self.action, PatchAction::Unchanged) {
                return format!(
                    "[dry run] section `{}` in {} is already up to date\n",
                    self.section, self.path
                );
            }
            return format!(
                "[dry run] would {} section `{}` in {}\n{}",
                self.action.infinitive(),
                self.section,
                self.path,
                self.diff
            );
        }
        let note = conformance_note(self.conformant);
        format!(
            "{} section `{}` in {}{note}\n",
            self.action.past(),
            self.section,
            self.path
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn bundle_with(content: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("tables")).unwrap();
        let path = dir.path().join("tables/orders.md");
        fs::write(&path, content).unwrap();
        (dir, path)
    }

    fn mode() -> OutputMode {
        OutputMode::test(crate::cli::Format::Text, false, "patch")
    }

    #[test]
    fn replaces_a_section_in_place() {
        let (dir, path) = bundle_with("---\ntype: Table\n---\n# Schema\n\n- id\n");
        let args = PatchArgs {
            section: "# Schema".to_owned(),
            content: Some("- id\n- total".to_owned()),
            ..Default::default()
        };
        run(dir.path(), "tables/orders", &args, mode()).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(
            written,
            "---\ntype: Table\n---\n# Schema\n\n- id\n- total\n"
        );
    }

    #[test]
    fn appends_to_a_section() {
        let (dir, path) = bundle_with("---\ntype: Table\n---\n# Joins\n\nfirst\n");
        let args = PatchArgs {
            section: "# Joins".to_owned(),
            content: Some("second".to_owned()),
            append: true,
            ..Default::default()
        };
        run(dir.path(), "tables/orders", &args, mode()).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(
            written,
            "---\ntype: Table\n---\n# Joins\n\nfirst\n\nsecond\n"
        );
    }

    #[test]
    fn creates_a_missing_section_at_end() {
        let (dir, path) = bundle_with("---\ntype: Table\n---\n# Schema\n\n- id\n");
        let args = PatchArgs {
            section: "# Joins".to_owned(),
            content: Some("orders.id = items.order_id".to_owned()),
            ..Default::default()
        };
        run(dir.path(), "tables/orders", &args, mode()).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(
            written,
            "---\ntype: Table\n---\n# Schema\n\n- id\n\n# Joins\n\norders.id = items.order_id\n"
        );
    }

    #[test]
    fn missing_concept_is_not_found() {
        let dir = TempDir::new().unwrap();
        let args = PatchArgs {
            section: "# Joins".to_owned(),
            content: Some("x".to_owned()),
            ..Default::default()
        };
        let err = run(dir.path(), "tables/ghost", &args, mode()).unwrap_err();
        assert!(matches!(err, Error::ConceptNotFound { .. }));
    }

    #[test]
    fn non_conformant_concept_is_gated_unless_forced() {
        let (dir, path) = bundle_with("---\ntitle: Orphan\n---\n# Notes\n\nhi\n");
        let args = PatchArgs {
            section: "# Notes".to_owned(),
            content: Some("changed".to_owned()),
            ..Default::default()
        };
        let err = run(dir.path(), "tables/orders", &args, mode()).unwrap_err();
        assert!(matches!(err, Error::NotConformant { .. }));

        let forced = PatchArgs {
            section: "# Notes".to_owned(),
            content: Some("changed".to_owned()),
            force: true,
            ..Default::default()
        };
        run(dir.path(), "tables/orders", &forced, mode()).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("changed"));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let (dir, path) = bundle_with("---\ntype: Table\n---\n# Schema\n\n- id\n");
        let args = PatchArgs {
            section: "# Schema".to_owned(),
            content: Some("- changed".to_owned()),
            dry_run: true,
            ..Default::default()
        };
        run(dir.path(), "tables/orders", &args, mode()).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, "---\ntype: Table\n---\n# Schema\n\n- id\n");
    }

    #[test]
    fn rewriting_identical_content_is_a_noop() {
        let (dir, path) = bundle_with("---\ntype: Table\n---\n# Schema\n\n- id\n");
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        let args = PatchArgs {
            section: "# Schema".to_owned(),
            content: Some("- id".to_owned()),
            ..Default::default()
        };
        run(dir.path(), "tables/orders", &args, mode()).unwrap();
        let after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn missing_content_is_an_input_error() {
        let (dir, _) = bundle_with("---\ntype: Table\n---\n# Schema\n\n- id\n");
        // No `--content` and stdin is the test harness's terminal-less pipe; the
        // command must refuse rather than silently blank the section.
        let args = PatchArgs {
            section: "# Schema".to_owned(),
            content: None,
            ..Default::default()
        };
        let err = run(dir.path(), "tables/orders", &args, mode()).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }
}
