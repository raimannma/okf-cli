//! Concept identity and OKF link resolution.
//!
//! A concept's identity is its file path within the bundle with the `.md`
//! extension removed (SPEC §2): `tables/users.md` → `tables/users`. IDs always
//! use `/` separators regardless of host platform, so bundles are portable.

use std::fmt;
use std::path::Path;

/// The canonical identifier of a concept: its bundle-relative path without `.md`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
pub struct ConceptId(String);

impl ConceptId {
    /// Build a concept ID from a path relative to the bundle root.
    ///
    /// Path components are joined with `/` and a trailing `.md` is stripped.
    /// Returns `None` if the path has no components.
    #[must_use]
    pub fn from_relative_path(path: &Path) -> Option<Self> {
        let mut parts = Vec::new();
        for component in path.components() {
            // Keep only the "normal" segments; a relative bundle path has no
            // root or prefix, and `.`/`..` should not appear in a walked path.
            if let std::path::Component::Normal(part) = component {
                parts.push(part.to_string_lossy().into_owned());
            }
        }

        Self::from_joined(&parts.join("/"))
    }

    /// Build an ID from `/`-joined path segments, stripping a trailing `.md`.
    /// Returns `None` when nothing remains.
    fn from_joined(joined: &str) -> Option<Self> {
        let id = joined.strip_suffix(".md").unwrap_or(joined);
        (!id.is_empty()).then(|| Self(id.to_owned()))
    }

    /// The ID as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Resolve a markdown link target written in `self`'s document to the
    /// concept ID it points at.
    ///
    /// Handles both OKF link forms (SPEC §5): bundle-relative absolute links
    /// (`/tables/customers.md`) and document-relative links (`./other.md`,
    /// `../m.md`). Returns `None` for links that do not address a bundle
    /// concept — external URLs, `mailto:`, and pure in-document fragments.
    ///
    /// The target need not exist: a resolved-but-absent ID is a legal broken
    /// link ("reference first, fill in later").
    #[must_use]
    pub fn resolve_link(&self, target: &str) -> Option<Self> {
        // Document-relative links resolve against the directory holding `self`.
        let base = self.0.rsplit_once('/').map_or("", |(dir, _)| dir);
        Self::resolve_in(base, target)
    }

    /// Resolve a markdown link with no enclosing document, i.e. as if it were
    /// written at the bundle root.
    ///
    /// Identical to [`ConceptId::resolve_link`] for absolute links
    /// (`/tables/x.md`); document-relative links (`tables/x.md`, `./x.md`) are
    /// interpreted from the root directory. Used when an agent has a link but no
    /// source concept to anchor it to.
    #[must_use]
    pub fn resolve_from_root(target: &str) -> Option<Self> {
        Self::resolve_in("", target)
    }

    /// Resolve `target` against `base_dir`, the `/`-joined directory prefix of
    /// the document the link appears in (empty for the bundle root).
    fn resolve_in(base_dir: &str, target: &str) -> Option<Self> {
        let target = target.trim();
        // Strip any fragment or query; OKF links address whole documents.
        let path = target
            .split(['#', '?'])
            .next()
            .unwrap_or(target)
            .trim_end_matches('/');
        if path.is_empty() {
            return None;
        }
        // External references are not bundle concepts.
        if path.contains("://") || path.starts_with("mailto:") {
            return None;
        }

        // Bundle-relative links resolve from the root (ignoring `base_dir`);
        // document-relative links resolve against the document's directory.
        let (prefix, rest) = match path.strip_prefix('/') {
            Some(absolute) => ("", absolute),
            None => (base_dir, path),
        };

        // Normalize `.` and `..` segments as we walk them.
        let mut normalized: Vec<&str> = Vec::new();
        for seg in prefix.split('/').chain(rest.split('/')) {
            match seg {
                "" | "." => {}
                ".." => {
                    normalized.pop();
                }
                other => normalized.push(other),
            }
        }

        Self::from_joined(&normalized.join("/"))
    }
}

impl fmt::Display for ConceptId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::path::PathBuf;

    fn id(s: &str) -> ConceptId {
        ConceptId(s.to_owned())
    }

    #[test]
    fn id_from_path_strips_md_and_uses_forward_slashes() {
        let got = ConceptId::from_relative_path(&PathBuf::from("tables/users.md")).unwrap();
        assert_eq!(got, id("tables/users"));
    }

    #[test]
    fn id_from_empty_path_is_none() {
        assert!(ConceptId::from_relative_path(&PathBuf::from("")).is_none());
    }

    #[test]
    fn resolves_bundle_relative_absolute_link() {
        let from = id("tables/orders");
        assert_eq!(
            from.resolve_link("/tables/customers.md"),
            Some(id("tables/customers"))
        );
    }

    #[test]
    fn resolves_document_relative_link() {
        let from = id("tables/orders");
        assert_eq!(
            from.resolve_link("./customers.md"),
            Some(id("tables/customers"))
        );
        assert_eq!(
            from.resolve_link("../metrics/revenue.md"),
            Some(id("metrics/revenue"))
        );
    }

    #[test]
    fn strips_fragment_and_query() {
        let from = id("a/b");
        assert_eq!(
            from.resolve_link("/tables/x.md#schema"),
            Some(id("tables/x"))
        );
    }

    #[test]
    fn external_links_do_not_resolve() {
        let from = id("a/b");
        assert!(from.resolve_link("https://example.com/x").is_none());
        assert!(from.resolve_link("mailto:a@b.com").is_none());
        assert!(from.resolve_link("#section").is_none());
    }

    #[test]
    fn resolve_from_root_handles_absolute_and_relative_links() {
        assert_eq!(
            ConceptId::resolve_from_root("/tables/customers.md"),
            Some(id("tables/customers"))
        );
        // A document-relative link with no source document anchors at the root.
        assert_eq!(
            ConceptId::resolve_from_root("tables/orders.md"),
            Some(id("tables/orders"))
        );
        assert_eq!(
            ConceptId::resolve_from_root("./metrics/wau.md"),
            Some(id("metrics/wau"))
        );
        // `..` cannot escape above the bundle root.
        assert_eq!(ConceptId::resolve_from_root("../../x.md"), Some(id("x")));
        assert!(ConceptId::resolve_from_root("https://example.com").is_none());
    }

    #[test]
    fn broken_link_still_resolves_to_an_id() {
        let from = id("a/b");
        assert_eq!(
            from.resolve_link("/does/not/exist.md"),
            Some(id("does/not/exist"))
        );
    }
}
