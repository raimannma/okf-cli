//! Shared plumbing for the subcommands that address a concept by id.
//!
//! Concept-id validation, path mapping, reading the prior file, writing safely,
//! rendering the document, and the unified-diff preview are identical across every
//! command that writes to the bundle, so they live here once. The Phase 2 contract
//! each mutating command upholds — preview with `--dry-run`, gate on conformance —
//! is built on these primitives. The read-only commands share the lighter
//! [`lookup_id`] for resolving a query id.

use std::collections::BTreeSet;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use crate::core::{Concept, ConceptId, Frontmatter};
use crate::error::{Error, Result};

/// Reserved filenames that are never concepts (SPEC §3.1); writing one is rejected.
const RESERVED_STEMS: [&str; 2] = ["index", "log"];

/// Validate a user-supplied concept id and turn it into a [`ConceptId`].
///
/// Rejects ids that are absolute, empty, or contain `.`/`..` segments — both to
/// give a clear error and to keep the write inside the bundle (the path is later
/// rebuilt from the sanitized id, so traversal cannot escape `root`). Targets
/// resolving to a reserved filename are rejected too.
///
/// # Errors
///
/// [`Error::InvalidInput`] for a malformed id, or [`Error::ReservedConcept`] for
/// an `index`/`log` target.
pub(crate) fn parse_concept_id(raw: &str) -> Result<ConceptId> {
    let trimmed = raw.trim();
    let invalid = trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..");
    if invalid {
        return Err(Error::InvalidInput(format!(
            "`{raw}` is not a valid concept id (use a bundle-relative path like `tables/orders`)"
        )));
    }

    let id = ConceptId::from_relative_path(Path::new(&format!("{trimmed}.md")))
        .ok_or_else(|| Error::InvalidInput(format!("`{raw}` is not a valid concept id")))?;

    let stem = id.as_str().rsplit('/').next().unwrap_or_default();
    if RESERVED_STEMS.contains(&stem) {
        return Err(Error::ReservedConcept { id: id.to_string() });
    }
    Ok(id)
}

/// Parse and dedup a list of user-supplied write-target ids, preserving the order
/// they were first given. The atomic multi-id commands (`rm`, `fmt`) validate the
/// whole batch up front, so a single bad id fails the run before anything happens.
///
/// # Errors
///
/// Any error [`parse_concept_id`] raises for an individual id.
pub(crate) fn parse_unique_ids(raw: &[String]) -> Result<Vec<ConceptId>> {
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    for r in raw {
        let id = parse_concept_id(r)?;
        if seen.insert(id.clone()) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Resolve a user-supplied id for a read-only lookup into a [`ConceptId`].
///
/// Unlike [`parse_concept_id`], this applies no reserved-name or traversal gating:
/// the query commands only read, so a `..` or reserved id simply fails to resolve
/// to a concept rather than risking an escaping write.
///
/// # Errors
///
/// [`Error::InvalidInput`] if `raw` is not a valid concept id.
pub(crate) fn lookup_id(raw: &str) -> Result<ConceptId> {
    ConceptId::from_relative_path(Path::new(raw))
        .ok_or_else(|| Error::InvalidInput(format!("`{raw}` is not a valid concept id")))
}

/// The on-disk path for a concept, built segment by segment from the sanitized
/// id so a `/`-joined id maps correctly on every platform.
pub(crate) fn concept_path(root: &Path, id: &ConceptId) -> PathBuf {
    let mut path = root.to_path_buf();
    let mut segments = id.as_str().split('/').peekable();
    while let Some(seg) = segments.next() {
        if segments.peek().is_some() {
            path.push(seg);
        } else {
            // The final segment is the filename; append `.md` without
            // `set_extension`, which would clobber a dotted stem (e.g. `v1.2`).
            path.push(format!("{seg}.md"));
        }
    }
    path
}

/// Read the concept file if it exists, mapping a real read failure (but not a
/// plain "not found") to an error.
///
/// # Errors
///
/// [`Error::WriteFile`] if the file exists but cannot be read.
pub(crate) fn read_existing(path: &Path, id: &ConceptId) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::WriteFile {
            id: id.to_string(),
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Read the verbatim content of a concept that must exist, mapping a missing file
/// to [`Error::ConceptNotFound`]. The shared read step for the commands that edit
/// an existing concept and for relinking citers in `mv`/`rm`.
///
/// # Errors
///
/// [`Error::ConceptNotFound`] if the file does not exist, or [`Error::WriteFile`]
/// if it exists but cannot be read.
pub(crate) fn read_prior(path: &Path, id: &ConceptId) -> Result<String> {
    read_existing(path, id)?.ok_or_else(|| Error::ConceptNotFound { id: id.to_string() })
}

/// Read and parse an existing concept, returning its verbatim prior content (for
/// diffs) alongside the parsed model. The shared read+parse step for the commands
/// that edit an existing concept (`patch`, `link`/`unlink`, `fmt`).
///
/// # Errors
///
/// [`Error::ConceptNotFound`] if the file does not exist, [`Error::ParseConcept`]
/// if it cannot be parsed, or [`Error::WriteFile`] if it exists but cannot be read.
pub(crate) fn load_source(path: &Path, id: &ConceptId) -> Result<(String, Concept)> {
    let prior = read_prior(path, id)?;
    let concept = Concept::parse(id.clone(), &prior).map_err(|source| Error::ParseConcept {
        id: id.to_string(),
        source,
    })?;
    Ok((prior, concept))
}

/// Render a full concept document: the `---`-delimited frontmatter followed by the
/// body, matching how `get --format text` reconstructs a concept.
///
/// # Errors
///
/// [`Error::RenderFrontmatter`] if the frontmatter cannot be serialized to YAML.
pub(crate) fn render_document(
    id: &ConceptId,
    frontmatter: &Frontmatter,
    body: &str,
) -> Result<String> {
    let yaml = frontmatter
        .to_yaml()
        .map_err(|source| Error::RenderFrontmatter {
            id: id.to_string(),
            source,
        })?;
    Ok(format!("---\n{yaml}---\n{body}"))
}

/// Enforce the conformance gate shared by the mutating commands on a parsed
/// [`Concept`]: it must carry a non-empty `type` (OKF §9) unless `force`. Returns
/// whether it conforms.
///
/// # Errors
///
/// [`Error::NotConformant`] when the concept has no non-empty `type` and `force`
/// is `false`.
pub(crate) fn require_conformant(concept: &Concept, force: bool) -> Result<bool> {
    require_conformant_fm(&concept.frontmatter, &concept.id, force, "edit")
}

/// The conformance gate on a bare [`Frontmatter`], for commands that gate before a
/// [`Concept`] exists (e.g. `set` building a new one). `verb` names the action in
/// the error message — "write", "edit", "format". Returns whether it conforms.
///
/// # Errors
///
/// [`Error::NotConformant`] when the frontmatter has no non-empty `type` and
/// `force` is `false`.
pub(crate) fn require_conformant_fm(
    frontmatter: &Frontmatter,
    id: &ConceptId,
    force: bool,
    verb: &str,
) -> Result<bool> {
    let conformant = frontmatter.type_().is_some();
    if !conformant && !force {
        return Err(Error::NotConformant {
            id: id.to_string(),
            reason: format!(
                "frontmatter has no non-empty `type` field (OKF §9); pass --force to {verb} anyway"
            ),
        });
    }
    Ok(conformant)
}

/// The trailing "(not conformant)" note shared by the human renderings of the
/// mutating commands; empty when the concept conforms.
pub(crate) fn conformance_note(conformant: bool) -> &'static str {
    if conformant {
        ""
    } else {
        "  (not conformant: no `type`)"
    }
}

/// Reconstruct a document after editing only its body, preserving the original
/// frontmatter bytes verbatim.
///
/// `prior` is the raw on-disk document and `old_body` the body a freshly parsed
/// [`Concept`] holds (a suffix of `prior`); the returned document is `prior` with
/// that suffix swapped for `new_body`. Unlike [`render_document`], this never
/// re-serializes the frontmatter, so an edit that only touches the body — like
/// `mv` rewriting an inbound link — leaves the YAML formatting untouched. Returns
/// `None` if `old_body` is not a suffix of `prior` (it always is for a concept
/// parsed from `prior`), letting the caller fall back to a full render.
pub(crate) fn with_replaced_body(prior: &str, old_body: &str, new_body: &str) -> Option<String> {
    let header = prior.strip_suffix(old_body)?;
    Some(format!("{header}{new_body}"))
}

/// Rebuild a concept's document after a body-only edit: swap `concept`'s body for
/// `new_body`, keeping the original frontmatter bytes verbatim where possible and
/// falling back to a full re-render. The shared core of the link-graph rewrites in
/// `mv` and `rm`.
///
/// # Errors
///
/// [`Error::RenderFrontmatter`] if the fallback re-render cannot serialize the
/// frontmatter.
pub(crate) fn rebuild_document(prior: &str, concept: &Concept, new_body: &str) -> Result<String> {
    match with_replaced_body(prior, &concept.body, new_body) {
        Some(doc) => Ok(doc),
        None => render_document(&concept.id, &concept.frontmatter, new_body),
    }
}

/// Ensure a non-empty body ends in exactly one trailing newline; leave an empty
/// body empty.
///
/// Keeps written files POSIX-clean and is the single normalization `set` and
/// `fmt` share, so a concept written by one is already canonical to the other.
pub(crate) fn normalize_body(body: &str) -> String {
    let trimmed = body.trim_end_matches('\n');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

/// Write `content` to `path`, creating parent directories as needed.
///
/// # Errors
///
/// [`Error::WriteFile`] on any directory-creation or write failure.
pub(crate) fn write_concept(path: &Path, id: &ConceptId, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::WriteFile {
            id: id.to_string(),
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, content).map_err(|source| Error::WriteFile {
        id: id.to_string(),
        path: path.to_path_buf(),
        source,
    })
}

/// The post-mutation link check shared by the writing commands: parse the
/// document `content` that was (or would be) written for `id` and return one
/// warning per outbound link that points at a concept missing from the bundle.
///
/// Target existence is decided by a filesystem check rather than a full bundle
/// load, so an idempotent edit stays cheap. A broken link is legal (SPEC §5: a
/// reference-first placeholder) — hence a warning, never an error — but the
/// caller surfaces it so a mutation that dangles a link does not pass silently.
/// Content that cannot be parsed yields no warnings; the command's own diff
/// already shows what was written.
pub(crate) fn broken_link_warnings(root: &Path, id: &ConceptId, content: &str) -> Vec<String> {
    let Ok(concept) = Concept::parse(id.clone(), content) else {
        return Vec::new();
    };
    crate::core::check::concept_broken_links(&concept, |target| {
        concept_path(root, target).is_file()
    })
    .into_iter()
    .map(|broken| {
        format!(
            "`{}` links to `{}`, which does not exist in the bundle \
             (broken link, left as a reference-first placeholder; SPEC §5)",
            broken.from, broken.target
        )
    })
    .collect()
}

/// A line-level unified diff of `before` → `after`, headed `a/<path>` `b/<path>`.
/// Empty when the two are identical.
pub(crate) fn unified_diff(before: &str, after: &str, path: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

/// Read the whole of standard input as a string, but only when it is piped — an
/// interactive terminal returns `None` rather than blocking on a read.
///
/// # Errors
///
/// [`Error::ReadStdin`] if reading piped input fails.
pub(crate) fn read_stdin_if_piped() -> Result<Option<String>> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut buf = String::new();
    stdin
        .lock()
        .read_to_string(&mut buf)
        .map_err(|source| Error::ReadStdin { source })?;
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn rejects_path_traversal_and_absolute_ids() {
        assert!(matches!(
            parse_concept_id("../escape"),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            parse_concept_id("/abs/path"),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(parse_concept_id(""), Err(Error::InvalidInput(_))));
    }

    #[test]
    fn rejects_reserved_targets() {
        assert!(matches!(
            parse_concept_id("tables/index"),
            Err(Error::ReservedConcept { .. })
        ));
        assert!(matches!(
            parse_concept_id("log"),
            Err(Error::ReservedConcept { .. })
        ));
        assert!(parse_concept_id("tables/orders").is_ok());
    }

    #[test]
    fn concept_path_appends_md_without_clobbering_dotted_stems() {
        let root = Path::new("/bundle");
        let id = parse_concept_id("metrics/v1.2").unwrap();
        assert_eq!(
            concept_path(root, &id),
            Path::new("/bundle/metrics/v1.2.md")
        );
    }

    #[test]
    fn render_document_reconstructs_a_concept() {
        let id = parse_concept_id("tables/orders").unwrap();
        let fm = Frontmatter::parse("type: Table\ntitle: Orders").unwrap();
        let doc = render_document(&id, &fm, "# Schema\n").unwrap();
        assert_eq!(doc, "---\ntype: Table\ntitle: Orders\n---\n# Schema\n");
    }

    #[test]
    fn with_replaced_body_preserves_frontmatter_bytes() {
        // The frontmatter keeps its exact bytes (quotes, folding) — only the body
        // suffix is swapped.
        let prior = "---\ntype: T\ntimestamp: '2026-05-28T00:00:00+00:00'\n---\nold body\n";
        let got = with_replaced_body(prior, "old body\n", "new body\n").unwrap();
        assert_eq!(
            got,
            "---\ntype: T\ntimestamp: '2026-05-28T00:00:00+00:00'\n---\nnew body\n"
        );
        // A body that is not a suffix yields None so the caller can fall back.
        assert!(with_replaced_body(prior, "not the body", "x").is_none());
    }

    #[test]
    fn normalize_body_collapses_trailing_newlines() {
        assert_eq!(normalize_body("# Body\n\n\n"), "# Body\n");
        assert_eq!(normalize_body("# Body"), "# Body\n");
        assert_eq!(normalize_body(""), "");
        assert_eq!(normalize_body("\n\n"), "");
    }

    #[test]
    fn unified_diff_headers_name_the_file() {
        let diff = unified_diff("a\n", "b\n", "tables/orders.md");
        assert!(diff.contains("--- a/tables/orders.md"));
        assert!(diff.contains("+++ b/tables/orders.md"));
        assert!(diff.contains("-a"));
        assert!(diff.contains("+b"));
        assert_eq!(unified_diff("same\n", "same\n", "x.md"), "");
    }
}
