//! `index` subcommand: (re)generate the reserved `index.md` directory listings.
//!
//! An `index.md` is fully derived from the concepts beneath its directory
//! (SPEC §6), so it can be regenerated from the bundle at any time. This command
//! brings every directory's listing in line with the bundle: rewriting the ones
//! that drifted, removing the ones for directories that no longer hold concepts,
//! and leaving up-to-date ones untouched.
//!
//! The same engine ([`compute`] + [`apply`]) is what the mutating commands call
//! to keep indexes in sync after they add, move, or remove concepts — so a
//! regeneration and an automatic post-mutation sync produce identical results.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use walkdir::WalkDir;

use crate::commands::mutate::unified_diff;
use crate::core::index::{ConceptSummary, INDEX_FILENAME, plan};
use crate::core::{Bundle, ConceptId};
use crate::error::{Error, Result};
use crate::output::{OutputMode, bundle_warnings};

/// Exit code when `--check` finds an out-of-date index (distinct from a hard error).
const DRIFT_EXIT_CODE: u8 = 5;

/// Options for `okf index`.
#[derive(Debug, Default)]
pub(crate) struct IndexArgs {
    /// Report drift and exit non-zero without writing anything (CI gate).
    pub check: bool,
    /// Preview changes as unified diffs without writing anything.
    pub dry_run: bool,
}

/// Regenerate every `index.md` under `root` from the bundle (or preview / check).
///
/// Returns [`ExitCode::SUCCESS`], except under `--check` when some index is out of
/// date, where it returns [`DRIFT_EXIT_CODE`] so CI can gate on it.
///
/// # Errors
///
/// [`Error::BundleNotADirectory`] if `root` is not a directory, or
/// [`Error::ReadIndex`] / [`Error::WriteIndex`] on I/O failure.
pub(crate) fn run(root: &Path, args: &IndexArgs, mode: OutputMode) -> Result<ExitCode> {
    let bundle = Bundle::load(root)?;
    let warnings = bundle_warnings(&bundle);
    let summaries: Vec<ConceptSummary> = bundle
        .concepts()
        .map(ConceptSummary::from_concept)
        .collect();

    let changes = compute(root, &summaries)?;
    let preview = args.check || args.dry_run;
    if !preview {
        apply(&changes)?;
    }

    let drift = !changes.is_empty();
    let output = IndexOutput {
        mode: if args.check {
            "check"
        } else if args.dry_run {
            "dry-run"
        } else {
            "regenerate"
        },
        written: !preview && drift,
        up_to_date: !drift,
        changed: changes.len(),
        changes,
    };
    mode.emit(&output, &output.render_human(), &warnings);

    Ok(if args.check && drift {
        ExitCode::from(DRIFT_EXIT_CODE)
    } else {
        ExitCode::SUCCESS
    })
}

/// Compute the index changes that bring `root`'s `index.md` files in line with
/// `summaries` (the desired post-state set of concepts).
///
/// Reads the current `index.md` files once, diffs them against the planned
/// listings, and returns one [`IndexChange`] per file that must be written or
/// removed — ordered by path, with up-to-date files omitted. Pure with respect to
/// the filesystem: nothing is written until [`apply`].
///
/// # Errors
///
/// [`Error::ReadIndex`] if an existing `index.md` cannot be read.
pub(crate) fn compute(root: &Path, summaries: &[ConceptSummary]) -> Result<Vec<IndexChange>> {
    let existing = read_existing_indexes(root)?;
    let planned = plan(summaries, &|path| existing.get(path).cloned());

    let mut changes = Vec::new();
    let mut planned_paths = std::collections::BTreeSet::new();
    for index in &planned {
        planned_paths.insert(index.path.clone());
        let prior = existing.get(&index.path).map_or("", String::as_str);
        if prior != index.content {
            changes.push(IndexChange {
                diff: unified_diff(prior, &index.content, &index.path),
                abs_path: join_rel(root, &index.path),
                new_content: Some(index.content.clone()),
                path: index.path.clone(),
                action: IndexAction::Written,
            });
        }
    }

    // An index for a directory that no longer holds concepts is stale: remove it
    // so the listing can't point at deleted concepts.
    for (path, content) in &existing {
        if !planned_paths.contains(path) {
            changes.push(IndexChange {
                diff: unified_diff(content, "", path),
                abs_path: join_rel(root, path),
                new_content: None,
                path: path.clone(),
                action: IndexAction::Removed,
            });
        }
    }

    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

/// Write the regenerated listings and remove the stale ones.
///
/// # Errors
///
/// [`Error::WriteIndex`] on any write, directory-creation, or removal failure.
pub(crate) fn apply(changes: &[IndexChange]) -> Result<()> {
    for change in changes {
        match &change.new_content {
            Some(content) => {
                if let Some(parent) = change.abs_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|source| Error::WriteIndex {
                        path: parent.to_path_buf(),
                        source,
                    })?;
                }
                std::fs::write(&change.abs_path, content).map_err(|source| Error::WriteIndex {
                    path: change.abs_path.clone(),
                    source,
                })?;
            }
            None => match std::fs::remove_file(&change.abs_path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Error::WriteIndex {
                        path: change.abs_path.clone(),
                        source,
                    });
                }
            },
        }
    }
    Ok(())
}

/// Append a regenerated-indexes summary (and, for a dry run, the diffs) to a
/// mutating command's human output. Renders nothing when no index changed.
pub(crate) fn append_human_summary(out: &mut String, changes: &[IndexChange], dry_run: bool) {
    use std::fmt::Write as _;

    if changes.is_empty() {
        return;
    }
    let lead = if dry_run {
        "would regenerate"
    } else {
        "regenerated"
    };
    let files = if changes.len() == 1 { "file" } else { "files" };
    let _ = writeln!(out, "  {lead} {} index {files}", changes.len());
    for change in changes {
        let verb = match change.action {
            IndexAction::Written => "updated",
            IndexAction::Removed => "removed",
        };
        let _ = writeln!(out, "    {verb} {}", change.path);
    }
    if dry_run {
        for change in changes {
            out.push_str(&change.diff);
        }
    }
}

/// Read every existing `index.md` under `root`, keyed by bundle-relative path.
fn read_existing_indexes(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut indexes = BTreeMap::new();
    for entry in WalkDir::new(root).sort_by_file_name() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() != INDEX_FILENAME {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let Some(id) = relative_slash_path(relative) else {
            continue;
        };
        let content = std::fs::read_to_string(entry.path()).map_err(|source| Error::ReadIndex {
            path: entry.path().to_path_buf(),
            source,
        })?;
        indexes.insert(id, content);
    }
    Ok(indexes)
}

/// A bundle-relative path with `/` separators (the form index paths use), or
/// `None` for a path with non-UTF-8 or non-normal components.
fn relative_slash_path(relative: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// Resolve a `/`-separated bundle-relative path to an absolute path under `root`,
/// segment by segment so it maps correctly on every platform.
fn join_rel(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in relative.split('/') {
        path.push(segment);
    }
    path
}

/// What [`compute`] decided to do to one `index.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum IndexAction {
    /// The listing drifted and was (or would be) rewritten.
    Written,
    /// The directory no longer holds concepts; its listing was (or would be) removed.
    Removed,
}

/// One `index.md` the regeneration writes or removes. `abs_path`/`new_content`
/// drive [`apply`] and are not part of the serialized contract.
#[derive(Debug, serde::Serialize)]
pub(crate) struct IndexChange {
    /// The index file's bundle-relative path.
    pub path: String,
    /// Whether it was written or removed.
    pub action: IndexAction,
    /// A unified diff of the change.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub diff: String,
    #[serde(skip)]
    abs_path: PathBuf,
    #[serde(skip)]
    new_content: Option<String>,
}

/// Bring the directory listings in line with a mutation: compute the index
/// changes for the post-state bundle (with `replaced` ids dropped and `added`
/// summaries inserted) and, unless `dry_run`, write them. Returns the changes for
/// the command's output and `--dry-run` preview.
///
/// Callers gate the call on their `--reindex` flag and pass an already-loaded
/// `bundle`, so a `--no-reindex` run pays for no extra work.
///
/// # Errors
///
/// [`Error::ReadIndex`] / [`Error::WriteIndex`] on I/O failure.
pub(crate) fn sync(
    root: &Path,
    bundle: &Bundle,
    replaced: &[ConceptId],
    added: Vec<ConceptSummary>,
    dry_run: bool,
) -> Result<Vec<IndexChange>> {
    let summaries = post_state(bundle, replaced, added);
    let changes = compute(root, &summaries)?;
    if !dry_run {
        apply(&changes)?;
    }
    Ok(changes)
}

/// Build the post-mutation concept summaries for a bundle in which `replaced` ids
/// are dropped and `added` summaries inserted — the desired index state after a
/// `set`/`move`/`remove` whose writes may not be on disk yet (e.g. `--dry-run`).
pub(crate) fn post_state(
    bundle: &Bundle,
    replaced: &[ConceptId],
    added: Vec<ConceptSummary>,
) -> Vec<ConceptSummary> {
    let mut summaries: Vec<ConceptSummary> = bundle
        .concepts()
        .filter(|c| !replaced.contains(&c.id) && !added.iter().any(|a| a.id == c.id))
        .map(ConceptSummary::from_concept)
        .collect();
    summaries.extend(added);
    summaries
}

/// The stable JSON contract for `okf index`.
#[derive(Debug, serde::Serialize)]
struct IndexOutput {
    /// The mode the command ran in: `regenerate`, `dry-run`, or `check`.
    mode: &'static str,
    /// Whether files were actually written (false for a dry run, check, or no-op).
    written: bool,
    /// Whether every index was already up to date (no change needed).
    up_to_date: bool,
    /// How many index files were (or would be) written or removed.
    changed: usize,
    /// Each index file that drifted, in path order; empty when all are current.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changes: Vec<IndexChange>,
}

impl IndexOutput {
    fn render_human(&self) -> String {
        use std::fmt::Write as _;

        if self.up_to_date {
            return "all index files are up to date\n".to_owned();
        }

        let mut out = String::new();
        let lead = match self.mode {
            "check" => "out of date",
            "dry-run" => "would regenerate",
            _ => "regenerated",
        };
        let files = if self.changed == 1 { "file" } else { "files" };
        let _ = writeln!(out, "{lead}: {} index {files}", self.changed);
        for change in &self.changes {
            let verb = match change.action {
                IndexAction::Written => "updated",
                IndexAction::Removed => "removed",
            };
            let _ = writeln!(out, "  {verb} {}", change.path);
        }
        if self.mode != "regenerate" {
            for change in &self.changes {
                out.push_str(&change.diff);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn mode() -> OutputMode {
        OutputMode::test(crate::cli::Format::Text, false, "index")
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
    fn regenerates_missing_indexes() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "tables/orders.md",
            "---\ntype: Table\ntitle: Orders\n---\nbody\n",
        );
        run(dir.path(), &IndexArgs::default(), mode()).unwrap();

        assert_eq!(
            read(&dir, "tables/index.md"),
            "# Table\n\n* [Orders](orders.md)\n"
        );
        assert_eq!(
            read(&dir, "index.md"),
            "# Subdirectories\n\n* [tables](tables/index.md)\n"
        );
    }

    #[test]
    fn regeneration_is_idempotent() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "tables/orders.md",
            "---\ntype: Table\ntitle: Orders\n---\nbody\n",
        );
        run(dir.path(), &IndexArgs::default(), mode()).unwrap();

        // A second pass finds nothing to do.
        let bundle = Bundle::load(dir.path()).unwrap();
        let summaries: Vec<_> = bundle
            .concepts()
            .map(ConceptSummary::from_concept)
            .collect();
        let changes = compute(dir.path(), &summaries).unwrap();
        assert!(changes.is_empty(), "second regeneration should be a no-op");
    }

    #[test]
    fn check_reports_drift_without_writing() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "tables/orders.md",
            "---\ntype: Table\ntitle: Orders\n---\nbody\n",
        );
        let code = run(
            dir.path(),
            &IndexArgs {
                check: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        assert_eq!(code, ExitCode::from(DRIFT_EXIT_CODE));
        assert!(!dir.path().join("tables/index.md").exists());
    }

    #[test]
    fn removes_stale_index_for_emptied_directory() {
        let dir = TempDir::new().unwrap();
        // An index for a directory with no concepts is stale and gets removed.
        write(
            &dir,
            "tables/orders.md",
            "---\ntype: Table\ntitle: Orders\n---\nbody\n",
        );
        write(&dir, "empty/index.md", "# Stale\n\n* [gone](gone.md)\n");
        run(dir.path(), &IndexArgs::default(), mode()).unwrap();
        assert!(!dir.path().join("empty/index.md").exists());
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "tables/orders.md",
            "---\ntype: Table\ntitle: Orders\n---\nbody\n",
        );
        run(
            dir.path(),
            &IndexArgs {
                dry_run: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        assert!(!dir.path().join("tables/index.md").exists());
    }
}
