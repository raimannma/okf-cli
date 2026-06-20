//! `link` / `unlink` subcommands: add or remove a cross-link, safely.
//!
//! These keep an agent from hand-writing markdown link syntax — the place a
//! generation most often goes wrong. `link` writes a `- [text](/to.md)` bullet
//! into a section of the source concept using the unambiguous bundle-relative
//! link form; `unlink` removes the bullets pointing at a target. Both uphold the
//! Phase 2 contract `set`/`patch` established: `--dry-run` previews a unified
//! diff, the write is gated on conformance unless `--force`, and an edit that
//! changes nothing is a no-op. `link` is idempotent — a concept that already
//! cites the target is left untouched.

use std::path::Path;

use crate::commands::mutate::{
    concept_path, conformance_note, load_source, parse_concept_id, read_existing, render_document,
    require_conformant, unified_diff, write_concept,
};
use crate::core::Concept;
use crate::error::Result;
use crate::output::OutputMode;

/// Options for `okf link`.
#[derive(Debug, Default)]
pub(crate) struct LinkArgs {
    /// The section to place the link bullet under (e.g. `# Related`).
    pub section: String,
    /// The link text; `None` defaults to the target's title, then its id.
    pub text: Option<String>,
    pub dry_run: bool,
    pub force: bool,
}

/// Options for `okf unlink`.
#[derive(Debug, Default)]
pub(crate) struct UnlinkArgs {
    pub dry_run: bool,
    pub force: bool,
}

/// Add a cross-link from `from` to `to` under `root` (or preview it).
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] / [`Error::ReservedConcept`] for a malformed
/// or reserved id, [`Error::ConceptNotFound`] if `from` does not exist (use `set`
/// to create it), [`Error::ParseConcept`] if `from` cannot be parsed,
/// [`Error::NotConformant`] when `from` has no non-empty `type` and `--force` was
/// not given, or [`Error::WriteFile`] / [`Error::RenderFrontmatter`] on I/O or
/// rendering failure.
pub(crate) fn link(
    root: &Path,
    from: &str,
    to: &str,
    args: &LinkArgs,
    mode: OutputMode,
) -> Result<()> {
    let from_id = parse_concept_id(from)?;
    let to_id = parse_concept_id(to)?;
    let from_path = concept_path(root, &from_id);
    let from_display = format!("{from_id}.md");

    let (prior, concept) = load_source(&from_path, &from_id)?;
    let conformant = require_conformant(&concept, args.force)?;

    // Resolve the target only to pick a default link text and decide whether to
    // warn about a dangling reference — never to forbid the link.
    let target = read_existing(&concept_path(root, &to_id), &to_id)?
        .and_then(|content| Concept::parse(to_id.clone(), &content).ok());
    let status = if target.is_some() {
        TargetStatus::Exists
    } else {
        TargetStatus::Missing
    };
    let text = args.text.clone().unwrap_or_else(|| {
        target
            .as_ref()
            .and_then(|c| c.frontmatter.title())
            .map_or_else(|| to_id.to_string(), str::to_owned)
    });

    let new_doc = if concept.links_to(&to_id) {
        prior.clone()
    } else {
        let item = format!("- [{text}](/{to_id}.md)");
        let new_content = match concept.section_content(&args.section) {
            Some(existing) if !existing.is_empty() => format!("{existing}\n{item}"),
            _ => item,
        };
        let (new_body, _) = concept.with_section(&args.section, &new_content, false);
        render_document(&from_id, &concept.frontmatter, &new_body)?
    };

    let changed = prior != new_doc;
    let written = !args.dry_run && changed;
    if written {
        write_concept(&from_path, &from_id, &new_doc)?;
    }

    let mut warnings = Vec::new();
    if status == TargetStatus::Missing {
        warnings.push(format!(
            "link target `{to_id}` does not exist in the bundle; left as a reference-first \
             placeholder (SPEC §5)"
        ));
    }

    let output = LinkOutput {
        from: from_id.to_string(),
        to: to_id.to_string(),
        path: from_display.clone(),
        section: args.section.clone(),
        text,
        target: status,
        action: if changed { "linked" } else { "unchanged" },
        written,
        dry_run: args.dry_run,
        conformant,
        diff: unified_diff(&prior, &new_doc, &from_display),
    };
    mode.emit(&output, &output.render_human(), &warnings);
    Ok(())
}

/// Remove every list-item link from `from` to `to` under `root` (or preview it).
///
/// # Errors
///
/// As [`link`], minus the target lookup: a malformed/reserved id, a missing or
/// unparseable `from`, a conformance failure, or an I/O / rendering error.
pub(crate) fn unlink(
    root: &Path,
    from: &str,
    to: &str,
    args: &UnlinkArgs,
    mode: OutputMode,
) -> Result<()> {
    let from_id = parse_concept_id(from)?;
    let to_id = parse_concept_id(to)?;
    let from_path = concept_path(root, &from_id);
    let from_display = format!("{from_id}.md");

    let (prior, concept) = load_source(&from_path, &from_id)?;
    let conformant = require_conformant(&concept, args.force)?;

    let (new_body, removed) = concept.without_link(&to_id);
    let new_doc = if removed == 0 {
        prior.clone()
    } else {
        render_document(&from_id, &concept.frontmatter, &new_body)?
    };

    let changed = prior != new_doc;
    let written = !args.dry_run && changed;
    if written {
        write_concept(&from_path, &from_id, &new_doc)?;
    }

    // A link can survive removal if it was written inline in prose rather than as
    // a bullet; flag it so the caller knows the citation is still there.
    let mut warnings = Vec::new();
    let inline_remains =
        Concept::parse(from_id.clone(), &new_doc).is_ok_and(|c| c.links_to(&to_id));
    if inline_remains {
        warnings.push(format!(
            "`{from_id}` still links to `{to_id}` inline (not as a list item); left untouched"
        ));
    }

    let output = UnlinkOutput {
        from: from_id.to_string(),
        to: to_id.to_string(),
        path: from_display.clone(),
        removed,
        action: if changed { "unlinked" } else { "unchanged" },
        written,
        dry_run: args.dry_run,
        conformant,
        diff: unified_diff(&prior, &new_doc, &from_display),
    };
    mode.emit(&output, &output.render_human(), &warnings);
    Ok(())
}

/// Whether a link's target concept is present in the bundle. A missing target is
/// legal — a reference-first placeholder (SPEC §5) — so this is reported, not an
/// error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum TargetStatus {
    Exists,
    Missing,
}

/// The stable JSON contract for `okf link`.
#[derive(Debug, serde::Serialize)]
struct LinkOutput {
    /// The concept the link was added to.
    from: String,
    /// The concept it now links to.
    to: String,
    /// The source concept's bundle-relative file path.
    path: String,
    /// The section the bullet was placed under.
    section: String,
    /// The link text written (or that already existed conceptually).
    text: String,
    /// Whether the target concept exists in the bundle or is a placeholder.
    target: TargetStatus,
    /// What happened: `linked` or `unchanged`. Under `--dry-run` this is what
    /// *would* happen (see `written`).
    action: &'static str,
    /// Whether the file was actually written (false for a dry run or a no-op).
    written: bool,
    /// Whether this invocation was a preview only.
    dry_run: bool,
    /// Whether the source concept satisfies OKF §9 (a non-empty `type`).
    conformant: bool,
    /// A unified diff of the change; empty when nothing changed.
    #[serde(skip_serializing_if = "str::is_empty")]
    diff: String,
}

impl LinkOutput {
    fn render_human(&self) -> String {
        if self.dry_run {
            if self.action == "unchanged" {
                return format!("[dry run] {} already links to {}\n", self.from, self.to);
            }
            return format!(
                "[dry run] would link {} → {} (in `{}`)\n{}",
                self.from, self.to, self.section, self.diff
            );
        }
        if self.action == "unchanged" {
            return format!("{} already links to {}\n", self.from, self.to);
        }
        let note = conformance_note(self.conformant);
        format!(
            "linked {} → {} (in `{}`){note}\n",
            self.from, self.to, self.section
        )
    }
}

/// The stable JSON contract for `okf unlink`.
#[derive(Debug, serde::Serialize)]
struct UnlinkOutput {
    /// The concept the link was removed from.
    from: String,
    /// The link target that was removed.
    to: String,
    /// The source concept's bundle-relative file path.
    path: String,
    /// How many list-item lines were removed.
    removed: usize,
    /// What happened: `unlinked` or `unchanged`. Under `--dry-run` this is what
    /// *would* happen (see `written`).
    action: &'static str,
    /// Whether the file was actually written (false for a dry run or a no-op).
    written: bool,
    /// Whether this invocation was a preview only.
    dry_run: bool,
    /// Whether the source concept satisfies OKF §9 (a non-empty `type`).
    conformant: bool,
    /// A unified diff of the change; empty when nothing changed.
    #[serde(skip_serializing_if = "str::is_empty")]
    diff: String,
}

impl UnlinkOutput {
    fn render_human(&self) -> String {
        if self.dry_run {
            if self.action == "unchanged" {
                return format!("[dry run] {} does not link to {}\n", self.from, self.to);
            }
            return format!(
                "[dry run] would unlink {} → {} (removing {})\n{}",
                self.from, self.to, self.removed, self.diff
            );
        }
        if self.action == "unchanged" {
            return format!("{} does not link to {}\n", self.from, self.to);
        }
        let note = conformance_note(self.conformant);
        format!(
            "unlinked {} → {} (removed {}){note}\n",
            self.from, self.to, self.removed
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::error::Error;
    use std::fs;
    use tempfile::TempDir;

    /// A bundle where `tables/orders` exists (conformant, no links) and
    /// `tables/customers` exists with a title.
    fn bundle() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("tables")).unwrap();
        fs::write(
            dir.path().join("tables/orders.md"),
            "---\ntype: Table\n---\n# Overview\n\norders table\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("tables/customers.md"),
            "---\ntype: Table\ntitle: Customers\n---\n# Overview\n\ncustomers\n",
        )
        .unwrap();
        dir
    }

    fn mode(command: &'static str) -> OutputMode {
        OutputMode::test(crate::cli::Format::Text, false, command)
    }

    fn read(dir: &TempDir, id: &str) -> String {
        fs::read_to_string(dir.path().join(format!("{id}.md"))).unwrap()
    }

    #[test]
    fn link_appends_a_bullet_using_target_title() {
        let dir = bundle();
        let args = LinkArgs {
            section: "# Related".to_owned(),
            ..Default::default()
        };
        link(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &args,
            mode("link"),
        )
        .unwrap();
        assert_eq!(
            read(&dir, "tables/orders"),
            "---\ntype: Table\n---\n# Overview\n\norders table\n\n\
             # Related\n\n- [Customers](/tables/customers.md)\n"
        );
    }

    #[test]
    fn link_into_existing_section_keeps_bullets_contiguous() {
        let dir = bundle();
        fs::write(
            dir.path().join("tables/orders.md"),
            "---\ntype: Table\n---\n# Related\n\n- [Items](/tables/items.md)\n",
        )
        .unwrap();
        let args = LinkArgs {
            section: "# Related".to_owned(),
            ..Default::default()
        };
        link(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &args,
            mode("link"),
        )
        .unwrap();
        assert_eq!(
            read(&dir, "tables/orders"),
            "---\ntype: Table\n---\n# Related\n\n\
             - [Items](/tables/items.md)\n- [Customers](/tables/customers.md)\n"
        );
    }

    #[test]
    fn link_honors_custom_section_and_text() {
        let dir = bundle();
        let args = LinkArgs {
            section: "# Metrics".to_owned(),
            text: Some("Customer Orders".to_owned()),
            ..Default::default()
        };
        link(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &args,
            mode("link"),
        )
        .unwrap();
        assert!(
            read(&dir, "tables/orders")
                .contains("# Metrics\n\n- [Customer Orders](/tables/customers.md)\n")
        );
    }

    #[test]
    fn link_is_idempotent() {
        let dir = bundle();
        let args = LinkArgs {
            section: "# Related".to_owned(),
            ..Default::default()
        };
        link(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &args,
            mode("link"),
        )
        .unwrap();
        let once = read(&dir, "tables/orders");
        let meta = fs::metadata(dir.path().join("tables/orders.md")).unwrap();
        let mtime = meta.modified().unwrap();
        link(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &args,
            mode("link"),
        )
        .unwrap();
        assert_eq!(read(&dir, "tables/orders"), once);
        // An unchanged second link does not rewrite the file.
        let after = fs::metadata(dir.path().join("tables/orders.md"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(mtime, after);
    }

    #[test]
    fn link_to_missing_target_still_writes() {
        let dir = bundle();
        let args = LinkArgs {
            section: "# Related".to_owned(),
            ..Default::default()
        };
        // The target does not exist; the link is created anyway (a placeholder),
        // and its text falls back to the id since there is no title to borrow.
        link(
            dir.path(),
            "tables/orders",
            "tables/ghost",
            &args,
            mode("link"),
        )
        .unwrap();
        assert!(read(&dir, "tables/orders").contains("- [tables/ghost](/tables/ghost.md)\n"));
    }

    #[test]
    fn link_dry_run_writes_nothing() {
        let dir = bundle();
        let before = read(&dir, "tables/orders");
        let args = LinkArgs {
            section: "# Related".to_owned(),
            dry_run: true,
            ..Default::default()
        };
        link(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &args,
            mode("link"),
        )
        .unwrap();
        assert_eq!(read(&dir, "tables/orders"), before);
    }

    #[test]
    fn link_missing_source_is_not_found() {
        let dir = bundle();
        let args = LinkArgs {
            section: "# Related".to_owned(),
            ..Default::default()
        };
        let err = link(
            dir.path(),
            "tables/ghost",
            "tables/customers",
            &args,
            mode("link"),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConceptNotFound { .. }));
    }

    #[test]
    fn link_non_conformant_source_is_gated_unless_forced() {
        let dir = bundle();
        fs::write(
            dir.path().join("tables/orders.md"),
            "---\ntitle: Orphan\n---\n# Overview\n\nx\n",
        )
        .unwrap();
        let args = LinkArgs {
            section: "# Related".to_owned(),
            ..Default::default()
        };
        let err = link(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &args,
            mode("link"),
        )
        .unwrap_err();
        assert!(matches!(err, Error::NotConformant { .. }));

        let forced = LinkArgs {
            section: "# Related".to_owned(),
            force: true,
            ..Default::default()
        };
        link(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &forced,
            mode("link"),
        )
        .unwrap();
        assert!(read(&dir, "tables/orders").contains("/tables/customers.md"));
    }

    #[test]
    fn unlink_removes_the_bullet() {
        let dir = bundle();
        fs::write(
            dir.path().join("tables/orders.md"),
            "---\ntype: Table\n---\n# Related\n\n\
             - [Customers](/tables/customers.md)\n- [Items](/tables/items.md)\n",
        )
        .unwrap();
        unlink(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &UnlinkArgs::default(),
            mode("unlink"),
        )
        .unwrap();
        assert_eq!(
            read(&dir, "tables/orders"),
            "---\ntype: Table\n---\n# Related\n\n- [Items](/tables/items.md)\n"
        );
    }

    #[test]
    fn unlink_is_a_noop_when_not_linked() {
        let dir = bundle();
        let path = dir.path().join("tables/orders.md");
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        unlink(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &UnlinkArgs::default(),
            mode("unlink"),
        )
        .unwrap();
        let after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn link_then_unlink_round_trips() {
        let dir = bundle();
        let before = read(&dir, "tables/orders");
        let link_args = LinkArgs {
            section: "# Related".to_owned(),
            ..Default::default()
        };
        link(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &link_args,
            mode("link"),
        )
        .unwrap();
        unlink(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &UnlinkArgs::default(),
            mode("unlink"),
        )
        .unwrap();
        // The bullet is gone; only the empty `# Related` section the link created
        // remains behind.
        assert!(!read(&dir, "tables/orders").contains("/tables/customers.md"));
        assert!(read(&dir, "tables/orders").starts_with(&before));
    }

    #[test]
    fn unlink_dry_run_writes_nothing() {
        let dir = bundle();
        let original = "---\ntype: Table\n---\n# Related\n\n- [C](/tables/customers.md)\n";
        fs::write(dir.path().join("tables/orders.md"), original).unwrap();
        let args = UnlinkArgs {
            dry_run: true,
            ..Default::default()
        };
        unlink(
            dir.path(),
            "tables/orders",
            "tables/customers",
            &args,
            mode("unlink"),
        )
        .unwrap();
        assert_eq!(read(&dir, "tables/orders"), original);
    }
}
