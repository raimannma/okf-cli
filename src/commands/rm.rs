//! `rm` subcommand: delete concepts and scrub every link to them.
//!
//! Deleting a concept by hand leaves the rest of the bundle citing a file that no
//! longer exists. `rm` removes each concept's file **and** drops every list-item
//! bullet across the bundle that links to a deleted concept — the bullet form
//! `link` writes and that "related" lists use. A link buried in prose is left as a
//! now-broken reference (legal "reference-first" placeholder, SPEC §5) and
//! reported as a warning rather than mangling a sentence.
//!
//! Like every Phase 2 mutation it previews with `--dry-run` and writes nothing.
//! There is no conformance gate — a deletion produces no concept to validate.
//! Instead `--force` carries the familiar `rm -f` meaning: by default a concept id
//! that does not exist is an error and nothing is deleted (the run is atomic);
//! with `--force`, missing ids are skipped.

use std::collections::BTreeSet;
use std::path::Path;

use crate::commands::index::{self, IndexChange};
use crate::commands::mutate::{
    concept_path, parse_unique_ids, read_existing, read_prior, rebuild_document, unified_diff,
    write_concept,
};
use crate::core::{Bundle, Concept, ConceptId};
use crate::error::{Error, Result};
use crate::output::{OutputMode, bundle_warnings};

/// Options for `okf remove`.
#[derive(Debug, Default)]
pub(crate) struct RmArgs {
    pub dry_run: bool,
    pub force: bool,
    /// Regenerate the affected `index.md` listings after removing (default on).
    pub reindex: bool,
}

/// Delete `ids` under `root`, scrubbing inbound bullet links (or preview it).
///
/// # Errors
///
/// [`Error::InvalidInput`] / [`Error::ReservedConcept`] for a malformed or
/// reserved id, [`Error::ConceptNotFound`] if a requested id does not exist and
/// `--force` was not given (nothing is deleted in that case), or
/// [`Error::WriteFile`] / [`Error::RenderFrontmatter`] on I/O or rendering failure.
pub(crate) fn run(root: &Path, ids: &[String], args: &RmArgs, mode: OutputMode) -> Result<()> {
    let targets = parse_unique_ids(ids)?;

    // Split into the concepts that exist (and will be deleted) and those that do
    // not. A missing id aborts the whole run unless --force, so a typo never
    // silently deletes a different concept than intended.
    let mut existing: Vec<ConceptId> = Vec::new();
    let mut missing: Vec<ConceptId> = Vec::new();
    for id in &targets {
        if read_existing(&concept_path(root, id), id)?.is_some() {
            existing.push(id.clone());
        } else {
            missing.push(id.clone());
        }
    }
    if let Some(first) = missing.first()
        && !args.force
    {
        return Err(Error::ConceptNotFound {
            id: first.to_string(),
        });
    }

    let delete_set: BTreeSet<ConceptId> = existing.iter().cloned().collect();
    let bundle = Bundle::load(root)?;
    let mut warnings = bundle_warnings(&bundle);

    let (relinked, relink_warnings) = relink_citers(root, &bundle, &existing, &delete_set)?;
    warnings.extend(relink_warnings);

    if !args.dry_run {
        // Scrub the citers first, then delete the files: a failure mid-way never
        // leaves a citer pointing at a file that is already gone.
        for change in &relinked {
            write_concept(&change.write_path, &change.write_id, &change.new_doc)?;
        }
        for id in &existing {
            let path = concept_path(root, id);
            std::fs::remove_file(&path).map_err(|source| Error::WriteFile {
                id: id.to_string(),
                path,
                source,
            })?;
        }
    }

    // Keep the directory listings in sync: a removed concept leaves its directory's
    // index, and a directory emptied of concepts loses its index entirely. The
    // post-state is the bundle minus the deleted concepts.
    let indexes = if args.reindex {
        index::sync(root, &bundle, &existing, Vec::new(), args.dry_run)?
    } else {
        Vec::new()
    };

    let links_removed = relinked.iter().map(|c| c.links_removed).sum();
    let output = RmOutput {
        written: !args.dry_run && (!existing.is_empty() || !relinked.is_empty()),
        dry_run: args.dry_run,
        concepts_deleted: existing.len(),
        files_relinked: relinked.len(),
        links_removed,
        removed: existing
            .iter()
            .map(|id| RemovedFile {
                id: id.to_string(),
                path: format!("{id}.md"),
            })
            .collect(),
        skipped: missing
            .iter()
            .map(|id| SkippedFile {
                id: id.to_string(),
                path: format!("{id}.md"),
                reason: "not found",
            })
            .collect(),
        files: relinked,
        indexes,
    };
    mode.emit(&output, &output.render_human(), &warnings);
    Ok(())
}

/// Build a [`FileChange`] for every concept (outside the delete set) that cites a
/// deleted concept, scrubbing its bullet links to them. Returns the changes and
/// warnings for citers whose only link is inline prose (left as a broken link).
fn relink_citers(
    root: &Path,
    bundle: &Bundle,
    deleted: &[ConceptId],
    delete_set: &BTreeSet<ConceptId>,
) -> Result<(Vec<FileChange>, Vec<String>)> {
    // Collect the distinct citers across all deleted concepts, skipping any that
    // are themselves being deleted, and order them for a deterministic result.
    let mut citer_ids: Vec<ConceptId> = Vec::new();
    let mut citer_seen = BTreeSet::new();
    for id in deleted {
        for citer in bundle.backlinks(id) {
            if !delete_set.contains(citer) && citer_seen.insert(citer.clone()) {
                citer_ids.push(citer.clone());
            }
        }
    }
    citer_ids.sort();

    let mut changes = Vec::new();
    let mut warnings = Vec::new();
    for citer_id in &citer_ids {
        let Some(citer) = bundle.get(citer_id) else {
            continue;
        };
        let (new_body, removed) = citer.without_links(deleted);
        let citer_path = concept_path(root, citer_id);
        let prior = read_prior(&citer_path, citer_id)?;
        let new_doc = if removed == 0 {
            prior.clone()
        } else {
            rebuild_document(&prior, citer, &new_body)?
        };

        // A link that survives the scrub was written inline in prose, not as a
        // bullet; flag it so the caller knows the citation is now a broken link.
        let survives = Concept::parse(citer_id.clone(), &new_doc)
            .is_ok_and(|c| deleted.iter().any(|t| c.links_to(t)));
        if survives {
            warnings.push(format!(
                "`{citer_id}` still links to a deleted concept inline (not as a list item); \
                 left as a broken link (SPEC §5)"
            ));
        }

        if removed > 0 {
            changes.push(FileChange {
                id: citer_id.to_string(),
                path: format!("{citer_id}.md"),
                links_removed: removed,
                diff: unified_diff(&prior, &new_doc, &format!("{citer_id}.md")),
                new_doc,
                write_path: citer_path,
                write_id: citer_id.clone(),
            });
        }
    }
    Ok((changes, warnings))
}

/// One citer whose bullet links to deleted concepts were scrubbed.
/// `new_doc`/`write_path`/`write_id` drive the write and are not serialized.
#[derive(Debug, serde::Serialize)]
struct FileChange {
    id: String,
    path: String,
    links_removed: usize,
    #[serde(skip_serializing_if = "str::is_empty")]
    diff: String,
    #[serde(skip)]
    new_doc: String,
    #[serde(skip)]
    write_path: std::path::PathBuf,
    #[serde(skip)]
    write_id: ConceptId,
}

/// A concept that was (or would be) deleted.
#[derive(Debug, serde::Serialize)]
struct RemovedFile {
    id: String,
    path: String,
}

/// A requested id that did not exist and was skipped under `--force`.
#[derive(Debug, serde::Serialize)]
struct SkippedFile {
    id: String,
    path: String,
    reason: &'static str,
}

/// The stable JSON contract for `okf rm`.
#[derive(Debug, serde::Serialize)]
struct RmOutput {
    /// Whether files were actually changed on disk (false for a dry run).
    written: bool,
    /// Whether this invocation was a preview only.
    dry_run: bool,
    /// How many concepts were deleted.
    concepts_deleted: usize,
    /// How many citing concepts had bullet links scrubbed.
    files_relinked: usize,
    /// Total bullet links removed across all citers.
    links_removed: usize,
    /// Each concept deleted, in the order requested.
    removed: Vec<RemovedFile>,
    /// Requested ids that did not exist (only populated under `--force`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    skipped: Vec<SkippedFile>,
    /// Each citer whose bullet links were scrubbed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<FileChange>,
    /// The `index.md` listings regenerated to keep up with the removals; empty
    /// when reindexing is off or nothing drifted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    indexes: Vec<IndexChange>,
}

impl RmOutput {
    fn render_human(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let lead = if self.dry_run {
            "[dry run] would remove"
        } else {
            "removed"
        };
        if self.removed.is_empty() {
            out.push_str("no concepts removed\n");
        }
        for file in &self.removed {
            let _ = writeln!(out, "{lead} {} ({})", file.id, file.path);
        }

        if self.files_relinked == 0 {
            if !self.removed.is_empty() {
                out.push_str("  no inbound links to scrub\n");
            }
        } else {
            let files = if self.files_relinked == 1 {
                "file"
            } else {
                "files"
            };
            let _ = writeln!(
                out,
                "  scrubbed {} link(s) across {} {files}",
                self.links_removed, self.files_relinked
            );
            for change in &self.files {
                let _ = writeln!(out, "    {} ({})", change.id, change.links_removed);
            }
        }

        for skip in &self.skipped {
            let _ = writeln!(out, "  skipped {} ({})", skip.id, skip.reason);
        }

        if self.dry_run {
            for change in &self.files {
                out.push_str(&change.diff);
            }
        }
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

    fn mode() -> OutputMode {
        OutputMode::test(crate::cli::Format::Text, false, "remove")
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

    /// `tables/orders` exists; `reports/daily` cites it as a bullet, and
    /// `reports/weekly` cites it inline in prose.
    fn bundle() -> TempDir {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "tables/orders.md",
            "---\ntype: Table\n---\n# Overview\n\norders\n",
        );
        write(
            &dir,
            "reports/daily.md",
            "---\ntype: Report\n---\n# Sources\n\n- [Orders](/tables/orders.md)\n- [Keep](/tables/keep.md)\n",
        );
        write(
            &dir,
            "reports/weekly.md",
            "---\ntype: Report\n---\nSee [orders](/tables/orders.md) inline.\n",
        );
        dir
    }

    #[test]
    fn deletes_file_and_scrubs_bullet_links() {
        let dir = bundle();
        run(
            dir.path(),
            &["tables/orders".to_owned()],
            &RmArgs::default(),
            mode(),
        )
        .unwrap();

        assert!(!dir.path().join("tables/orders.md").exists());
        // The bullet pointing at the deleted concept is gone; the other stays.
        let daily = read(&dir, "reports/daily.md");
        assert!(!daily.contains("/tables/orders.md"));
        assert!(daily.contains("- [Keep](/tables/keep.md)\n"));
        // The inline prose link survives as a (now broken) reference.
        assert!(read(&dir, "reports/weekly.md").contains("[orders](/tables/orders.md)"));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = bundle();
        let before = read(&dir, "reports/daily.md");
        run(
            dir.path(),
            &["tables/orders".to_owned()],
            &RmArgs {
                dry_run: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        assert!(dir.path().join("tables/orders.md").exists());
        assert_eq!(read(&dir, "reports/daily.md"), before);
    }

    #[test]
    fn missing_id_aborts_without_deleting_anything() {
        let dir = bundle();
        let err = run(
            dir.path(),
            &["tables/orders".to_owned(), "tables/ghost".to_owned()],
            &RmArgs::default(),
            mode(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConceptNotFound { .. }));
        // The existing concept is untouched — the run is atomic.
        assert!(dir.path().join("tables/orders.md").exists());
    }

    #[test]
    fn force_skips_missing_ids() {
        let dir = bundle();
        run(
            dir.path(),
            &["tables/orders".to_owned(), "tables/ghost".to_owned()],
            &RmArgs {
                force: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        assert!(!dir.path().join("tables/orders.md").exists());
    }

    #[test]
    fn deletes_multiple_and_scrubs_each() {
        let dir = TempDir::new().unwrap();
        write(&dir, "a.md", "---\ntype: T\n---\na\n");
        write(&dir, "b.md", "---\ntype: T\n---\nb\n");
        write(
            &dir,
            "hub.md",
            "---\ntype: T\n---\n# Related\n\n- [A](/a.md)\n- [B](/b.md)\n",
        );
        run(
            dir.path(),
            &["a".to_owned(), "b".to_owned()],
            &RmArgs::default(),
            mode(),
        )
        .unwrap();
        assert!(!dir.path().join("a.md").exists());
        assert!(!dir.path().join("b.md").exists());
        let hub = read(&dir, "hub.md");
        assert!(!hub.contains("/a.md"));
        assert!(!hub.contains("/b.md"));
    }

    #[test]
    fn citer_in_delete_set_is_not_relinked() {
        // `a` and `b` link to each other; deleting both must not error trying to
        // relink a file that is itself being removed.
        let dir = TempDir::new().unwrap();
        write(&dir, "a.md", "---\ntype: T\n---\n- [B](/b.md)\n");
        write(&dir, "b.md", "---\ntype: T\n---\n- [A](/a.md)\n");
        run(
            dir.path(),
            &["a".to_owned(), "b".to_owned()],
            &RmArgs::default(),
            mode(),
        )
        .unwrap();
        assert!(!dir.path().join("a.md").exists());
        assert!(!dir.path().join("b.md").exists());
    }

    #[test]
    fn reindex_drops_the_entry_and_emptied_directory_index() {
        let dir = TempDir::new().unwrap();
        write(&dir, "tables/orders.md", "---\ntype: Table\n---\norders\n");
        write(
            &dir,
            "tables/customers.md",
            "---\ntype: Table\n---\ncustomers\n",
        );
        // First lay down the indexes by reindexing on a no-op-ish removal.
        run(
            dir.path(),
            &["tables/orders".to_owned()],
            &RmArgs {
                reindex: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        // `orders` is gone from the listing; `customers` remains.
        let tables = read(&dir, "tables/index.md");
        assert_eq!(tables, "# Table\n\n* [customers](customers.md)\n");

        // Removing the last concept in the directory removes its index too.
        run(
            dir.path(),
            &["tables/customers".to_owned()],
            &RmArgs {
                reindex: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        assert!(!dir.path().join("tables/index.md").exists());
    }

    #[test]
    fn reserved_id_is_rejected() {
        let dir = bundle();
        let err = run(
            dir.path(),
            &["tables/index".to_owned()],
            &RmArgs::default(),
            mode(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ReservedConcept { .. }));
    }

    #[test]
    fn duplicate_ids_delete_once() {
        let dir = bundle();
        run(
            dir.path(),
            &["tables/orders".to_owned(), "tables/orders".to_owned()],
            &RmArgs::default(),
            mode(),
        )
        .unwrap();
        assert!(!dir.path().join("tables/orders.md").exists());
    }
}
