//! `fmt` subcommand: normalize concepts to their canonical on-disk form.
//!
//! Agents write sloppy — flow-style YAML, ragged indentation, stray trailing
//! blank lines — and `fmt` hands back the canonical file: the frontmatter parsed
//! and re-serialized (key order preserved, SPEC §4.1) and the body kept verbatim
//! save for a single trailing newline. The normalization is exactly the one `set`
//! applies, so a concept written by either command is already clean to the other.
//!
//! Unlike `set`/`patch`, `fmt` is read-only by default: it previews the change as
//! a unified diff and writes nothing until `-w`. A run over several ids is atomic
//! — every concept is parsed and gated first, so one unparseable or non-conformant
//! file means nothing is written.

use std::path::{Path, PathBuf};

use crate::commands::mutate::{
    concept_path, conformance_note, load_source, normalize_body, parse_unique_ids, render_document,
    require_conformant_fm, unified_diff, write_concept,
};
use crate::core::ConceptId;
use crate::error::Result;
use crate::output::OutputMode;

/// Options for `okf fmt`.
#[derive(Debug, Default)]
pub(crate) struct FmtArgs {
    /// Write the canonical form back to each file; without it, `fmt` only previews.
    pub write: bool,
    /// Format even a concept that has no non-empty `type` (OKF §9).
    pub force: bool,
}

/// Normalize each concept in `ids` under `root`, previewing or (with `--write`)
/// writing the canonical form.
///
/// # Errors
///
/// [`Error::InvalidInput`] / [`Error::ReservedConcept`] for a malformed or
/// reserved id, [`Error::ConceptNotFound`] if a requested id does not exist,
/// [`Error::ParseConcept`] if a file cannot be parsed, [`Error::NotConformant`]
/// when a concept has no non-empty `type` and `--force` was not given, or
/// [`Error::WriteFile`] / [`Error::RenderFrontmatter`] on I/O or rendering
/// failure. Any of these aborts the run before anything is written.
pub(crate) fn run(root: &Path, ids: &[String], args: &FmtArgs, mode: OutputMode) -> Result<()> {
    let targets = parse_unique_ids(ids)?;

    // Compute every change first; a failure here leaves the bundle untouched, so a
    // multi-file run never half-formats.
    let mut planned: Vec<PlannedFmt> = Vec::with_capacity(targets.len());
    for id in &targets {
        planned.push(plan(root, id, args.force)?);
    }

    // All concepts passed the gate; now apply the writes if asked.
    if args.write {
        for change in planned.iter().filter(|c| c.changed) {
            write_concept(&change.path, &change.id, &change.new_doc)?;
        }
    }

    let formatted = planned.iter().filter(|c| c.changed).count();
    let output = FmtOutput {
        write: args.write,
        written: args.write && formatted > 0,
        formatted,
        files: planned.into_iter().map(FmtFile::from).collect(),
    };
    mode.emit(&output, &output.render_human(), &[]);
    Ok(())
}

/// A single concept's planned reformat: what it is, whether it changed, the diff,
/// and the canonical bytes to write.
struct PlannedFmt {
    id: ConceptId,
    path: PathBuf,
    path_display: String,
    changed: bool,
    conformant: bool,
    diff: String,
    new_doc: String,
}

/// Parse the concept at `id`, gate it on conformance, and render its canonical
/// form — without touching disk.
fn plan(root: &Path, id: &ConceptId, force: bool) -> Result<PlannedFmt> {
    let path = concept_path(root, id);
    let path_display = format!("{id}.md");

    let (prior, concept) = load_source(&path, id)?;
    let conformant = require_conformant_fm(&concept.frontmatter, id, force, "format")?;

    let new_doc = render_document(id, &concept.frontmatter, &normalize_body(&concept.body))?;
    let changed = prior != new_doc;
    let diff = unified_diff(&prior, &new_doc, &path_display);

    Ok(PlannedFmt {
        id: id.clone(),
        path,
        path_display,
        changed,
        conformant,
        diff,
        new_doc,
    })
}

/// One concept's entry in the `fmt` result.
#[derive(Debug, serde::Serialize)]
struct FmtFile {
    /// The concept ID.
    concept: String,
    /// Its bundle-relative file path.
    path: String,
    /// `formatted` if its bytes changed, else `unchanged`. Under a preview (no
    /// `-w`) this is what *would* happen.
    action: &'static str,
    /// Whether the concept satisfies OKF §9 (a non-empty `type`). Only ever false
    /// when `--force` let a non-conformant concept through.
    conformant: bool,
    /// A unified diff of the normalization; empty when the file is already canonical.
    #[serde(skip_serializing_if = "str::is_empty")]
    diff: String,
}

impl From<PlannedFmt> for FmtFile {
    fn from(p: PlannedFmt) -> Self {
        Self {
            concept: p.id.to_string(),
            path: p.path_display,
            action: if p.changed { "formatted" } else { "unchanged" },
            conformant: p.conformant,
            diff: p.diff,
        }
    }
}

/// The stable JSON contract for `okf fmt`.
#[derive(Debug, serde::Serialize)]
struct FmtOutput {
    /// Whether `-w` was given (a writing run rather than a preview).
    write: bool,
    /// Whether any file was actually changed on disk (false for a preview or a
    /// run where everything was already canonical).
    written: bool,
    /// How many concepts needed (or, in a preview, would need) reformatting.
    formatted: usize,
    /// Each concept, in the order requested.
    files: Vec<FmtFile>,
}

impl FmtOutput {
    fn render_human(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let total = self.files.len();
        let files = if total == 1 { "file" } else { "files" };

        if self.write {
            if self.formatted == 0 {
                let _ = writeln!(out, "all {total} {files} already formatted");
            } else {
                let _ = writeln!(out, "formatted {} of {total} {files}", self.formatted);
                for file in self.files.iter().filter(|f| f.action == "formatted") {
                    let note = conformance_note(file.conformant);
                    let _ = writeln!(out, "  {}{note}", file.path);
                }
            }
            return out;
        }

        // Preview: report what would change and show the diffs.
        if self.formatted == 0 {
            let _ = writeln!(out, "[preview] all {total} {files} already formatted");
            return out;
        }
        let _ = writeln!(
            out,
            "[preview] {} of {total} {files} would be reformatted (pass -w to write)",
            self.formatted
        );
        for file in self.files.iter().filter(|f| f.action == "formatted") {
            out.push_str(&file.diff);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::error::Error;
    use std::fs;
    use tempfile::TempDir;

    fn mode() -> OutputMode {
        OutputMode::test(crate::cli::Format::Text, false, "fmt")
    }

    fn write(dir: &TempDir, rel: &str, content: &str) {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn read(dir: &TempDir, rel: &str) -> String {
        fs::read_to_string(dir.path().join(rel)).unwrap()
    }

    #[test]
    fn write_canonicalizes_frontmatter_and_trailing_newlines() {
        let dir = TempDir::new().unwrap();
        // Flow-style frontmatter and ragged trailing blank lines.
        write(
            &dir,
            "tables/orders.md",
            "---\n{type: Table, title: Orders}\n---\n# Schema\n\n- id\n\n\n",
        );
        run(
            dir.path(),
            &["tables/orders".to_owned()],
            &FmtArgs {
                write: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        assert_eq!(
            read(&dir, "tables/orders.md"),
            "---\ntype: Table\ntitle: Orders\n---\n# Schema\n\n- id\n"
        );
    }

    #[test]
    fn default_is_a_preview_that_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let original = "---\n{type: Table}\n---\nbody\n\n\n";
        write(&dir, "tables/orders.md", original);
        run(
            dir.path(),
            &["tables/orders".to_owned()],
            &FmtArgs::default(),
            mode(),
        )
        .unwrap();
        assert_eq!(read(&dir, "tables/orders.md"), original);
    }

    #[test]
    fn already_canonical_file_is_unchanged() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tables/orders.md");
        write(
            &dir,
            "tables/orders.md",
            "---\ntype: Table\n---\n# Schema\n",
        );
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        run(
            dir.path(),
            &["tables/orders".to_owned()],
            &FmtArgs {
                write: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        // Nothing to do: the file is not rewritten (mtime preserved).
        let after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn fmt_output_matches_what_set_writes() {
        // The normalization `fmt` applies is exactly `set`'s, so formatting a file
        // and re-setting it land on the same bytes.
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "tables/orders.md",
            "---\n{type: Table, owner: data-team}\n---\nbody",
        );
        run(
            dir.path(),
            &["tables/orders".to_owned()],
            &FmtArgs {
                write: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        assert_eq!(
            read(&dir, "tables/orders.md"),
            "---\ntype: Table\nowner: data-team\n---\nbody\n"
        );
    }

    #[test]
    fn missing_concept_is_not_found() {
        let dir = TempDir::new().unwrap();
        let err = run(
            dir.path(),
            &["tables/ghost".to_owned()],
            &FmtArgs::default(),
            mode(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConceptNotFound { .. }));
    }

    #[test]
    fn unparseable_file_is_a_parse_error() {
        let dir = TempDir::new().unwrap();
        write(&dir, "x.md", "# No frontmatter here\n");
        let err = run(dir.path(), &["x".to_owned()], &FmtArgs::default(), mode()).unwrap_err();
        assert!(matches!(err, Error::ParseConcept { .. }));
    }

    #[test]
    fn non_conformant_concept_is_gated_unless_forced() {
        let dir = TempDir::new().unwrap();
        write(&dir, "notes/x.md", "---\n{title: Orphan}\n---\nbody\n");

        let err = run(
            dir.path(),
            &["notes/x".to_owned()],
            &FmtArgs {
                write: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::NotConformant { .. }));
        // Gated before any write — the sloppy frontmatter is left as-is.
        assert!(read(&dir, "notes/x.md").contains("{title: Orphan}"));

        run(
            dir.path(),
            &["notes/x".to_owned()],
            &FmtArgs {
                write: true,
                force: true,
            },
            mode(),
        )
        .unwrap();
        assert_eq!(read(&dir, "notes/x.md"), "---\ntitle: Orphan\n---\nbody\n");
    }

    #[test]
    fn run_is_atomic_across_multiple_ids() {
        let dir = TempDir::new().unwrap();
        write(&dir, "a.md", "---\n{type: T}\n---\nbody\n\n");
        write(&dir, "b.md", "# missing frontmatter\n");
        // `b` fails to parse, so `a` must not be formatted either.
        let before = read(&dir, "a.md");
        let err = run(
            dir.path(),
            &["a".to_owned(), "b".to_owned()],
            &FmtArgs {
                write: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ParseConcept { .. }));
        assert_eq!(read(&dir, "a.md"), before);
    }

    #[test]
    fn reserved_id_is_rejected() {
        let dir = TempDir::new().unwrap();
        let err = run(
            dir.path(),
            &["tables/index".to_owned()],
            &FmtArgs::default(),
            mode(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ReservedConcept { .. }));
    }

    #[test]
    fn duplicate_ids_format_once() {
        let dir = TempDir::new().unwrap();
        write(&dir, "a.md", "---\n{type: T}\n---\nbody\n");
        run(
            dir.path(),
            &["a".to_owned(), "a".to_owned()],
            &FmtArgs {
                write: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        assert_eq!(read(&dir, "a.md"), "---\ntype: T\n---\nbody\n");
    }
}
