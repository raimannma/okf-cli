//! `resolve` subcommand: canonicalize a markdown link to a concept ID.
//!
//! Agents constantly hold a link they found in a concept body and need its
//! canonical ID — and whether the target actually exists — without
//! reimplementing OKF's link-resolution rules (SPEC §5). This command answers
//! exactly that: it applies the same resolver the bundle loader uses, then
//! reports the resulting ID plus whether the bundle loaded it.
//!
//! A document-relative link (`./other.md`, `../m.md`) only has meaning relative
//! to the document it appears in, so `--from <concept-id>` supplies that anchor.
//! Without it, the link is resolved as if written at the bundle root. Absolute
//! links (`/tables/x.md`) resolve the same either way.

use std::path::Path;

use crate::commands::mutate::lookup_id;
use crate::core::{Bundle, ConceptId};
use crate::error::Result;
use crate::output::OutputMode;

/// Resolve `link` (optionally anchored in concept `from`) against the bundle at
/// `path` and report its canonical ID and existence.
///
/// A link that does not address a bundle concept — an external URL, `mailto:`,
/// or a bare `#fragment` — is **not** an error: it is reported with a null
/// `concept`. A resolved-but-absent concept is likewise reported with
/// `exists: false`, since broken links are first-class in OKF.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] if `from` is not a valid concept id,
/// [`Error::BundleNotADirectory`] if `path` is not a readable bundle, or
/// [`Error::Json`] if JSON output cannot be serialized.
pub(crate) fn run(path: &Path, link: &str, from: Option<&str>, mode: OutputMode) -> Result<()> {
    let from_id = match from {
        Some(raw) => Some(lookup_id(raw)?),
        None => None,
    };

    let bundle = Bundle::load(path)?;
    let output = ResolveOutput::build(&bundle, link, from_id);
    let warnings = crate::output::bundle_warnings(&bundle);

    mode.emit(&output, &output.render_human(), &warnings);
    Ok(())
}

/// The stable JSON contract for `okf resolve`.
#[derive(Debug, serde::Serialize)]
struct ResolveOutput {
    /// The link target as the user supplied it.
    input: String,
    /// The concept the link was anchored in, or null when resolved from root.
    from: Option<String>,
    /// The canonical concept ID the link resolves to, or null when the link
    /// does not address a bundle concept (external URL, `mailto:`, fragment).
    concept: Option<String>,
    /// Whether that concept is actually loaded in the bundle.
    exists: bool,
}

impl ResolveOutput {
    /// Resolve `link` (anchored in `from`, or from the bundle root when `None`)
    /// against `bundle` into the output record.
    fn build(bundle: &Bundle, link: &str, from: Option<ConceptId>) -> Self {
        let resolved = match &from {
            Some(id) => id.resolve_link(link),
            None => ConceptId::resolve_from_root(link),
        };
        Self {
            input: link.to_owned(),
            from: from.map(|id| id.to_string()),
            concept: resolved.as_ref().map(ToString::to_string),
            exists: resolved.as_ref().is_some_and(|id| bundle.contains(id)),
        }
    }

    fn render_human(&self) -> String {
        let target = match &self.concept {
            Some(concept) => {
                let status = if self.exists { "exists" } else { "missing" };
                format!("{concept}  ({status})")
            }
            None => "(not a bundle link)".to_owned(),
        };
        match &self.from {
            Some(from) => format!("{} → {target}  [from {from}]\n", self.input),
            None => format!("{} → {target}\n", self.input),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A small bundle: tables/orders exists; tables/ghost is only ever linked.
    fn fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("tables")).unwrap();
        fs::write(
            root.join("tables/orders.md"),
            "---\ntype: Table\n---\nlinks [ghost](/tables/ghost.md)\n",
        )
        .unwrap();
        fs::write(
            root.join("tables/customers.md"),
            "---\ntype: Table\n---\nno links\n",
        )
        .unwrap();
        dir
    }

    fn build(dir: &TempDir, link: &str, from: Option<&str>) -> ResolveOutput {
        let from_id = from.map(|raw| ConceptId::from_relative_path(Path::new(raw)).unwrap());
        let bundle = Bundle::load(dir.path()).unwrap();
        ResolveOutput::build(&bundle, link, from_id)
    }

    #[test]
    fn absolute_link_resolves_to_existing_concept() {
        let dir = fixture();
        let out = build(&dir, "/tables/customers.md", None);
        assert_eq!(out.concept.as_deref(), Some("tables/customers"));
        assert!(out.exists);
    }

    #[test]
    fn document_relative_link_uses_the_from_anchor() {
        let dir = fixture();
        // `./customers.md` only resolves to tables/customers when anchored in a
        // tables/ document.
        let out = build(&dir, "./customers.md", Some("tables/orders"));
        assert_eq!(out.concept.as_deref(), Some("tables/customers"));
        assert!(out.exists);
        assert_eq!(out.from.as_deref(), Some("tables/orders"));
    }

    #[test]
    fn relative_link_without_anchor_resolves_from_root() {
        let dir = fixture();
        let out = build(&dir, "tables/customers.md", None);
        assert_eq!(out.concept.as_deref(), Some("tables/customers"));
        assert!(out.exists);
        assert!(out.from.is_none());
    }

    #[test]
    fn broken_link_resolves_but_does_not_exist() {
        let dir = fixture();
        let out = build(&dir, "/tables/ghost.md", None);
        assert_eq!(out.concept.as_deref(), Some("tables/ghost"));
        assert!(!out.exists);
    }

    #[test]
    fn external_link_is_not_a_bundle_concept() {
        let dir = fixture();
        let out = build(&dir, "https://example.com/x", None);
        assert!(out.concept.is_none());
        assert!(!out.exists);
    }

    #[test]
    fn human_output_is_readable() {
        let dir = fixture();
        let out = build(&dir, "./customers.md", Some("tables/orders"));
        insta::assert_snapshot!(
            out.render_human(),
            @"./customers.md → tables/customers  (exists)  [from tables/orders]
        "
        );
    }

    #[test]
    fn json_contract_is_stable() {
        let dir = fixture();
        let out = build(&dir, "/tables/ghost.md", None);
        let json = serde_json::to_string_pretty(&out).unwrap();
        insta::assert_snapshot!(json, @r#"
        {
          "input": "/tables/ghost.md",
          "from": null,
          "concept": "tables/ghost",
          "exists": false
        }
        "#);
    }
}
