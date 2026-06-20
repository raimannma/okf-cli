//! Bundle integrity checks: the broken-link detector shared by the `okf check`
//! command and the post-mutation warnings the writing commands emit.
//!
//! A *broken link* is an outbound markdown link that resolves to a concept ID
//! absent from the bundle (SPEC §5: legal as a "reference-first" placeholder,
//! but worth surfacing). A link to an external URL or a `#fragment` resolves to
//! no concept and is never broken.

use std::collections::BTreeSet;

use crate::core::Bundle;
use crate::core::concept::Concept;
use crate::core::concept_id::ConceptId;

/// An outbound link whose resolved target is not present in the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenLink {
    /// The concept the link is written in.
    pub from: ConceptId,
    /// The concept ID the link resolves to but which does not exist.
    pub target: ConceptId,
    /// The link destination exactly as written in the document.
    pub raw: String,
}

/// Every broken outbound link across the bundle, ordered by source concept
/// (then by the order the links appear in each document).
#[must_use]
pub fn broken_links(bundle: &Bundle) -> Vec<BrokenLink> {
    let mut out = Vec::new();
    for concept in bundle.concepts() {
        out.extend(concept_broken_links(concept, |id| bundle.contains(id)));
    }
    out
}

/// The broken outbound links of a single `concept`, deciding target existence
/// with `exists`. Deduplicated by target (a document may link to the same
/// missing concept more than once), keeping the first-written form.
///
/// A link that resolves to the concept itself is never reported — a self-link
/// always has a present target.
#[must_use]
pub fn concept_broken_links(
    concept: &Concept,
    exists: impl Fn(&ConceptId) -> bool,
) -> Vec<BrokenLink> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for link in &concept.links {
        let Some(target) = &link.target else { continue };
        if *target == concept.id || exists(target) {
            continue;
        }
        if seen.insert(target.clone()) {
            out.push(BrokenLink {
                from: concept.id.clone(),
                target: target.clone(),
                raw: link.raw.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn id(s: &str) -> ConceptId {
        ConceptId::from_relative_path(Path::new(&format!("{s}.md"))).unwrap()
    }

    #[test]
    fn reports_links_to_missing_concepts_only() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "a.md",
            "---\ntype: T\n---\nlinks [b](/b.md), [ghost](/ghost.md), and [ext](https://e.com)\n",
        );
        write(dir.path(), "b.md", "---\ntype: T\n---\nno links\n");

        let bundle = Bundle::load(dir.path()).unwrap();
        let broken = broken_links(&bundle);
        assert_eq!(broken.len(), 1);
        let first = broken.first().unwrap();
        assert_eq!(first.from, id("a"));
        assert_eq!(first.target, id("ghost"));
        assert_eq!(first.raw, "/ghost.md");
    }

    #[test]
    fn dedupes_repeated_links_to_the_same_missing_target() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "a.md",
            "---\ntype: T\n---\n[x](/gone.md) then [x again](/gone.md)\n",
        );
        let bundle = Bundle::load(dir.path()).unwrap();
        assert_eq!(broken_links(&bundle).len(), 1);
    }

    #[test]
    fn concept_broken_links_uses_the_existence_predicate() {
        let c = Concept::parse(id("t"), "---\ntype: T\n---\n[a](/a.md) [b](/b.md)\n").unwrap();
        // `a` exists, `b` does not.
        let broken = concept_broken_links(&c, |t| *t == id("a"));
        assert_eq!(broken.len(), 1);
        assert_eq!(broken.first().unwrap().target, id("b"));
    }

    #[test]
    fn a_self_link_is_never_broken() {
        let c = Concept::parse(id("t"), "---\ntype: T\n---\nsee [me](/t.md)\n").unwrap();
        assert!(concept_broken_links(&c, |_| false).is_empty());
    }
}
