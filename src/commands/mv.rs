//! `mv` subcommand: rename or move a concept and repair the link graph.
//!
//! A rename is exactly where hand-editing a bundle breaks: the file moves but
//! every `[text](/old.md)` pointing at it is left dangling. `mv` renames the file
//! **and** rewrites every inbound link across the bundle to the destination, in
//! the canonical bundle-relative form (`/new-id.md`), so the graph stays intact.
//! It also re-anchors the moved file's own document-relative outbound links when
//! the move changes its directory, so links written as `../x.md` keep pointing at
//! the same concept.
//!
//! Like every Phase 2 mutation it previews with `--dry-run`, gates on conformance
//! of the moved concept unless `--force`, and never overwrites: a destination that
//! already exists is an error, not a clobber.

use std::path::Path;

use crate::commands::index::{self, IndexChange};
use crate::commands::mutate::{
    concept_path, conformance_note, load_source, parse_concept_id, read_existing, read_prior,
    rebuild_document, render_document, require_conformant, unified_diff, with_replaced_body,
    write_concept,
};
use crate::core::index::ConceptSummary;
use crate::core::{Bundle, ConceptId};
use crate::error::{Error, Result};
use crate::output::{OutputMode, bundle_warnings};

/// Options for `okf move`.
#[derive(Debug, Default)]
pub(crate) struct MvArgs {
    pub dry_run: bool,
    pub force: bool,
    /// Regenerate the affected `index.md` listings after moving (default on).
    pub reindex: bool,
}

/// Rename/move concept `old` to `new` under `root`, rewriting inbound links (or
/// preview it).
///
/// # Errors
///
/// [`Error::InvalidInput`] for a malformed id or a no-op rename to the same id,
/// [`Error::ReservedConcept`] for an `index`/`log` target, [`Error::ConceptNotFound`]
/// if `old` does not exist, [`Error::ConceptExists`] if `new` already exists,
/// [`Error::ParseConcept`] if `old` cannot be parsed, [`Error::NotConformant`] when
/// `old` has no non-empty `type` and `--force` was not given, or
/// [`Error::WriteFile`] / [`Error::RenderFrontmatter`] on I/O or rendering failure.
pub(crate) fn run(
    root: &Path,
    old: &str,
    new: &str,
    args: &MvArgs,
    mode: OutputMode,
) -> Result<()> {
    let old_id = parse_concept_id(old)?;
    let new_id = parse_concept_id(new)?;
    if old_id == new_id {
        return Err(Error::InvalidInput(format!(
            "`{old_id}` and `{new_id}` are the same concept; nothing to move"
        )));
    }

    let old_path = concept_path(root, &old_id);
    let new_path = concept_path(root, &new_id);

    let (prior, concept) = load_source(&old_path, &old_id)?;
    let conformant = require_conformant(&concept, args.force)?;

    if read_existing(&new_path, &new_id)?.is_some() {
        return Err(Error::ConceptExists {
            id: new_id.to_string(),
        });
    }

    // Re-anchor the moved file's own outbound links: a destination that would no
    // longer resolve to the same concept from the new location is rewritten to its
    // canonical absolute form. Same-directory renames leave these untouched.
    let (moved_body, moved_rewrites) = concept.rewrite_link_dests(|raw, resolved| {
        let target = resolved?;
        let desired = if *target == old_id { &new_id } else { target };
        if new_id.resolve_link(raw).as_ref() == Some(desired) {
            None
        } else {
            Some(format!("/{desired}.md"))
        }
    });
    // Preserve the frontmatter bytes verbatim — the move only edits the body.
    let moved_doc = match with_replaced_body(&prior, &concept.body, &moved_body) {
        Some(doc) => doc,
        None => render_document(&new_id, &concept.frontmatter, &moved_body)?,
    };

    let mut changes = Vec::new();
    changes.push(FileChange {
        id: new_id.to_string(),
        path: format!("{new_id}.md"),
        kind: ChangeKind::Renamed,
        links_rewritten: moved_rewrites,
        diff: unified_diff(&prior, &moved_doc, &format!("{old_id}.md → {new_id}.md")),
        new_doc: moved_doc,
        write_path: new_path.clone(),
        write_id: new_id.clone(),
    });

    // Rewrite inbound links in every concept that cites `old`, to `/new.md`.
    let bundle = Bundle::load(root)?;
    let (citer_changes, warnings) = relink_citers(root, &bundle, &old_id, &new_id)?;
    changes.extend(citer_changes);

    if !args.dry_run {
        // Write the relinked citers and the new file, then drop the old one, so a
        // failure mid-way never leaves the destination missing.
        for change in &changes {
            write_concept(&change.write_path, &change.write_id, &change.new_doc)?;
        }
        std::fs::remove_file(&old_path).map_err(|source| Error::WriteFile {
            id: old_id.to_string(),
            path: old_path.clone(),
            source,
        })?;
    }

    // Keep the directory listings in sync: the moved concept leaves its old
    // directory's index and joins the new one. The post-state maps `old_id` to
    // `new_id` so the preview is correct even under --dry-run.
    let indexes = if args.reindex {
        let summary = ConceptSummary::from_frontmatter(new_id.clone(), &concept.frontmatter);
        index::sync(
            root,
            &bundle,
            std::slice::from_ref(&old_id),
            vec![summary],
            args.dry_run,
        )?
    } else {
        Vec::new()
    };

    let relinked = changes
        .iter()
        .filter(|c| c.kind == ChangeKind::Relinked)
        .count();
    let links_rewritten = changes.iter().map(|c| c.links_rewritten).sum();
    let output = MvOutput {
        from: old_id.to_string(),
        to: new_id.to_string(),
        from_path: format!("{old_id}.md"),
        to_path: format!("{new_id}.md"),
        written: !args.dry_run,
        dry_run: args.dry_run,
        conformant,
        files_relinked: relinked,
        links_rewritten,
        files: changes,
        indexes,
    };
    mode.emit(&output, &output.render_human(), &warnings);
    Ok(())
}

/// Build a [`FileChange`] for every concept that cites `old_id`, rewriting its
/// inbound links to `/new_id.md`. Returns the changes and any warnings (a bundle
/// load skip, or a citer whose link is a non-inline form we cannot rewrite).
fn relink_citers(
    root: &Path,
    bundle: &Bundle,
    old_id: &ConceptId,
    new_id: &ConceptId,
) -> Result<(Vec<FileChange>, Vec<String>)> {
    let mut changes = Vec::new();
    let mut warnings = bundle_warnings(bundle);
    let new_dest = format!("/{new_id}.md");
    for citing_id in bundle.backlinks(old_id) {
        if citing_id == old_id {
            continue;
        }
        let Some(citing) = bundle.get(citing_id) else {
            continue;
        };
        let (new_body, n) = citing
            .rewrite_link_dests(|_, resolved| (resolved == Some(old_id)).then(|| new_dest.clone()));
        if n == 0 {
            warnings.push(format!(
                "`{citing_id}` links to `{old_id}` only via a non-inline form (reference-style \
                 or autolink); left unchanged — fix it by hand"
            ));
            continue;
        }
        let citing_path = concept_path(root, citing_id);
        let citing_prior = read_prior(&citing_path, citing_id)?;
        let citing_doc = rebuild_document(&citing_prior, citing, &new_body)?;
        changes.push(FileChange {
            id: citing_id.to_string(),
            path: format!("{citing_id}.md"),
            kind: ChangeKind::Relinked,
            links_rewritten: n,
            diff: unified_diff(&citing_prior, &citing_doc, &format!("{citing_id}.md")),
            new_doc: citing_doc,
            write_path: citing_path,
            write_id: citing_id.clone(),
        });
    }
    Ok((changes, warnings))
}

/// What `mv` did to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum ChangeKind {
    /// The concept itself — its file was renamed (and outbound links re-anchored).
    Renamed,
    /// A citing concept whose inbound links to the moved concept were rewritten.
    Relinked,
}

/// One file touched by the move. `new_doc`/`write_path`/`write_id` drive the write
/// and are not part of the serialized contract.
#[derive(Debug, serde::Serialize)]
struct FileChange {
    id: String,
    path: String,
    kind: ChangeKind,
    links_rewritten: usize,
    #[serde(skip_serializing_if = "str::is_empty")]
    diff: String,
    #[serde(skip)]
    new_doc: String,
    #[serde(skip)]
    write_path: std::path::PathBuf,
    #[serde(skip)]
    write_id: ConceptId,
}

/// The stable JSON contract for `okf mv`.
#[derive(Debug, serde::Serialize)]
struct MvOutput {
    /// The concept's id before the move.
    from: String,
    /// The concept's id after the move.
    to: String,
    /// The source file's bundle-relative path.
    from_path: String,
    /// The destination file's bundle-relative path.
    to_path: String,
    /// Whether files were actually written (false for a dry run).
    written: bool,
    /// Whether this invocation was a preview only.
    dry_run: bool,
    /// Whether the moved concept satisfies OKF §9 (a non-empty `type`).
    conformant: bool,
    /// How many citing concepts had inbound links rewritten.
    files_relinked: usize,
    /// Total link destinations rewritten (inbound plus the moved file's own).
    links_rewritten: usize,
    /// Every file the move touches: the renamed concept first, then each citer.
    files: Vec<FileChange>,
    /// The `index.md` listings regenerated to keep up with the move; empty when
    /// reindexing is off or nothing drifted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    indexes: Vec<IndexChange>,
}

impl MvOutput {
    fn render_human(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let lead = if self.dry_run {
            "[dry run] would move"
        } else {
            "moved"
        };
        let note = conformance_note(self.conformant);
        let _ = writeln!(
            out,
            "{lead} {} → {} ({}){note}",
            self.from, self.to, self.to_path
        );

        if self.files_relinked == 0 {
            out.push_str("  no inbound links to rewrite\n");
        } else {
            let files = if self.files_relinked == 1 {
                "file"
            } else {
                "files"
            };
            let inbound = self.links_rewritten.saturating_sub(self.moved_rewrites());
            let _ = writeln!(
                out,
                "  rewrote {inbound} inbound link(s) across {} {files}",
                self.files_relinked
            );
            for change in self.files.iter().filter(|c| c.kind == ChangeKind::Relinked) {
                let _ = writeln!(out, "    {} ({})", change.id, change.links_rewritten);
            }
        }
        if self.moved_rewrites() > 0 {
            let _ = writeln!(
                out,
                "  re-anchored {} link(s) in the moved file",
                self.moved_rewrites()
            );
        }

        if self.dry_run {
            for change in &self.files {
                out.push_str(&change.diff);
            }
        }
        index::append_human_summary(&mut out, &self.indexes, self.dry_run);
        out
    }

    /// Links rewritten inside the moved file itself (the `Renamed` entry).
    fn moved_rewrites(&self) -> usize {
        self.files
            .iter()
            .find(|c| c.kind == ChangeKind::Renamed)
            .map_or(0, |c| c.links_rewritten)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn mode() -> OutputMode {
        OutputMode::test(crate::cli::Format::Text, false, "move")
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

    /// `tables/orders` exists and `reports/daily` cites it with an absolute link;
    /// `reports/weekly` cites it twice with a relative link.
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
            "---\ntype: Report\n---\n# Sources\n\n- [Orders](/tables/orders.md)\n",
        );
        write(
            &dir,
            "reports/weekly.md",
            "---\ntype: Report\n---\nSee [orders](../tables/orders.md) and again \
             [o](../tables/orders.md).\n",
        );
        dir
    }

    #[test]
    fn moves_file_and_rewrites_inbound_links() {
        let dir = bundle();
        run(
            dir.path(),
            "tables/orders",
            "facts/orders",
            &MvArgs::default(),
            mode(),
        )
        .unwrap();

        // The file moved.
        assert!(!dir.path().join("tables/orders.md").exists());
        assert_eq!(
            read(&dir, "facts/orders.md"),
            "---\ntype: Table\n---\n# Overview\n\norders\n"
        );
        // Both citers now point at the new id, in canonical absolute form.
        assert!(read(&dir, "reports/daily.md").contains("- [Orders](/facts/orders.md)\n"));
        let weekly = read(&dir, "reports/weekly.md");
        assert!(weekly.contains("[orders](/facts/orders.md)"));
        assert!(weekly.contains("[o](/facts/orders.md)"));
        assert!(!weekly.contains("../tables/orders.md"));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = bundle();
        let before_daily = read(&dir, "reports/daily.md");
        run(
            dir.path(),
            "tables/orders",
            "facts/orders",
            &MvArgs {
                dry_run: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        assert!(dir.path().join("tables/orders.md").exists());
        assert!(!dir.path().join("facts/orders.md").exists());
        assert_eq!(read(&dir, "reports/daily.md"), before_daily);
    }

    #[test]
    fn re_anchors_the_moved_files_own_relative_links() {
        let dir = TempDir::new().unwrap();
        // `a/x` links to `a/y` via a sibling-relative link; moving it to `b/x`
        // would break that link unless it is re-anchored.
        write(
            &dir,
            "a/x.md",
            "---\ntype: T\n---\nsee [y](./y.md) and [ext](https://e.com)\n",
        );
        write(&dir, "a/y.md", "---\ntype: T\n---\ny\n");
        run(dir.path(), "a/x", "b/x", &MvArgs::default(), mode()).unwrap();
        let moved = read(&dir, "b/x.md");
        assert!(
            moved.contains("[y](/a/y.md)"),
            "relative link re-anchored: {moved}"
        );
        // External links are untouched.
        assert!(moved.contains("[ext](https://e.com)"));
    }

    #[test]
    fn same_directory_rename_leaves_relative_links_alone() {
        let dir = TempDir::new().unwrap();
        write(&dir, "a/x.md", "---\ntype: T\n---\nsee [y](./y.md)\n");
        write(&dir, "a/y.md", "---\ntype: T\n---\ny\n");
        run(dir.path(), "a/x", "a/z", &MvArgs::default(), mode()).unwrap();
        // Still in `a/`, so the relative link resolves the same and is untouched.
        assert_eq!(read(&dir, "a/z.md"), "---\ntype: T\n---\nsee [y](./y.md)\n");
    }

    #[test]
    fn missing_source_is_not_found() {
        let dir = bundle();
        let err = run(
            dir.path(),
            "tables/ghost",
            "facts/ghost",
            &MvArgs::default(),
            mode(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConceptNotFound { .. }));
    }

    #[test]
    fn existing_destination_is_refused() {
        let dir = bundle();
        write(&dir, "facts/orders.md", "---\ntype: Table\n---\nexisting\n");
        let err = run(
            dir.path(),
            "tables/orders",
            "facts/orders",
            &MvArgs::default(),
            mode(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConceptExists { .. }));
        // The destination is left untouched.
        assert_eq!(
            read(&dir, "facts/orders.md"),
            "---\ntype: Table\n---\nexisting\n"
        );
    }

    #[test]
    fn same_id_is_rejected() {
        let dir = bundle();
        let err = run(
            dir.path(),
            "tables/orders",
            "tables/orders",
            &MvArgs::default(),
            mode(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn non_conformant_source_is_gated_unless_forced() {
        let dir = TempDir::new().unwrap();
        write(&dir, "a.md", "---\ntitle: No Type\n---\nbody\n");
        let err = run(dir.path(), "a", "b", &MvArgs::default(), mode()).unwrap_err();
        assert!(matches!(err, Error::NotConformant { .. }));

        run(
            dir.path(),
            "a",
            "b",
            &MvArgs {
                force: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        assert!(dir.path().join("b.md").exists());
    }

    #[test]
    fn reindex_moves_the_entry_between_directory_listings() {
        let dir = bundle();
        // Seed the source directory's index so the move must update it.
        run(
            dir.path(),
            "tables/orders",
            "facts/orders",
            &MvArgs {
                reindex: true,
                ..Default::default()
            },
            mode(),
        )
        .unwrap();
        // The concept now appears under its new directory's index...
        assert_eq!(
            read(&dir, "facts/index.md"),
            "# Table\n\n* [orders](orders.md)\n"
        );
        // ...and the old directory, emptied of concepts, loses its index.
        assert!(!dir.path().join("tables/index.md").exists());
    }

    #[test]
    fn reserved_destination_is_rejected() {
        let dir = bundle();
        let err = run(
            dir.path(),
            "tables/orders",
            "tables/index",
            &MvArgs::default(),
            mode(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ReservedConcept { .. }));
    }
}
