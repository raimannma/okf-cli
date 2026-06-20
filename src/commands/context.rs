//! `context` subcommand: assemble a self-contained context slice for a concept.
//!
//! The differentiator (Phase 5): given a concept, pack the concept itself plus the
//! concepts it links to — out to `--depth` hops — into a single blob ready to drop
//! into a model's context window. Where `neighbors` returns only IDs, `context`
//! returns full documents, traversing the *outbound* graph (what the concept needs
//! to be understood), nearest first. Referenced concepts that the bundle never
//! loaded (broken links) can't be inlined, so they are reported separately rather
//! than silently dropped.
//!
//! Cost is the one-time bundle load plus a breadth-first walk of the reachable
//! neighborhood, so it scales with the size of the slice and the requested depth,
//! not with the size of the bundle.

use std::path::Path;

use crate::commands::mutate::{lookup_id, render_document};
use crate::core::{Bundle, ConceptId, Frontmatter};
use crate::error::Result;
use crate::output::OutputMode;

/// Assemble the context slice rooted at `concept_id` from the bundle at `path`,
/// following outbound links out to `depth` hops.
///
/// A root that resolves but is not loaded is **not** an error: it is reported with
/// `exists: false` and an empty slice (its outbound links are unknowable without
/// its body), consistent with how `neighbors` treats a broken target.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`](crate::error::Error::InvalidInput) if
/// `concept_id` is not a valid concept id,
/// [`Error::BundleNotADirectory`](crate::error::Error::BundleNotADirectory) if
/// `path` is not a readable bundle, or
/// [`Error::RenderFrontmatter`](crate::error::Error::RenderFrontmatter) if a
/// concept's frontmatter cannot be re-serialized for the markdown rendering.
pub(crate) fn run(path: &Path, concept_id: &str, depth: usize, mode: OutputMode) -> Result<()> {
    let id = lookup_id(concept_id)?;

    let bundle = Bundle::load(path)?;
    let output = ContextOutput::build(&bundle, &id, depth);
    let text = output.render_markdown()?;
    let warnings = crate::output::bundle_warnings(&bundle);

    mode.emit(&output, &text, &warnings);
    Ok(())
}

/// The stable JSON contract for `okf context`.
#[derive(Debug, serde::Serialize)]
struct ContextOutput<'a> {
    /// The concept the slice was rooted at.
    root: String,
    /// Whether that concept is actually loaded in the bundle.
    exists: bool,
    /// The maximum hop count followed along outbound links.
    depth: usize,
    /// The concepts in the slice, the root first (distance 0) then nearest first.
    concepts: Vec<ContextConcept<'a>>,
    /// Referenced concept IDs the bundle never loaded (broken links), sorted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing: Vec<String>,
}

/// One concept in the slice: its identity, how far it sits from the root, and its
/// full content. Serialized directly (no intermediate `Value`) so frontmatter key
/// order is preserved, mirroring `get`.
#[derive(Debug, serde::Serialize)]
struct ContextConcept<'a> {
    id: ConceptId,
    /// Hops from the root concept (0 = the root itself, 1 = a direct link).
    distance: usize,
    frontmatter: &'a Frontmatter,
    body: &'a str,
}

impl<'a> ContextOutput<'a> {
    fn build(bundle: &'a Bundle, root: &ConceptId, depth: usize) -> Self {
        let (included, missing) = collect(bundle, root, depth);

        let concepts = included
            .into_iter()
            .filter_map(|(id, distance)| {
                let concept = bundle.get(&id)?;
                Some(ContextConcept {
                    id,
                    distance,
                    frontmatter: &concept.frontmatter,
                    body: &concept.body,
                })
            })
            .collect();

        Self {
            root: root.to_string(),
            exists: bundle.contains(root),
            depth,
            concepts,
            missing: missing.iter().map(ConceptId::to_string).collect(),
        }
    }

    /// Render the slice as a single markdown blob: a header, then each concept's
    /// full document under a `=== id ===` marker (annotated with its depth for the
    /// non-root concepts), then any referenced-but-missing IDs.
    ///
    /// # Errors
    ///
    /// [`Error::RenderFrontmatter`](crate::error::Error::RenderFrontmatter) if a
    /// concept's frontmatter cannot be serialized to YAML.
    fn render_markdown(&self) -> Result<String> {
        use std::fmt::Write as _;

        let mut out = String::new();
        let _ = writeln!(out, "# Context: {} (depth {})", self.root, self.depth);
        if !self.exists {
            let _ = writeln!(out, "\n(root concept `{}` is missing)", self.root);
        }

        for concept in &self.concepts {
            out.push('\n');
            if concept.distance == 0 {
                let _ = writeln!(out, "=== {} ===", concept.id);
            } else {
                let _ = writeln!(out, "=== {} (depth {}) ===", concept.id, concept.distance);
            }
            let doc = render_document(&concept.id, concept.frontmatter, concept.body)?;
            out.push_str(&doc);
            if !doc.ends_with('\n') {
                out.push('\n');
            }
        }

        if !self.missing.is_empty() {
            out.push_str("\n=== referenced but missing ===\n");
            for id in &self.missing {
                let _ = writeln!(out, "{id}");
            }
        }

        Ok(out)
    }
}

/// Breadth-first walk from `start` along outbound links, out to `depth` hops.
///
/// Returns the loaded concepts to include (the root at distance 0 when it exists,
/// then each reachable concept at its shortest distance) and the referenced IDs
/// the bundle never loaded. Each distinct ID is visited once, so the cost is
/// linear in the edges of the visited neighborhood. Both lists are sorted
/// deterministically — included by (distance, id) so the root leads, missing by id.
fn collect(
    bundle: &Bundle,
    start: &ConceptId,
    depth: usize,
) -> (Vec<(ConceptId, usize)>, Vec<ConceptId>) {
    // Expand only through concepts the bundle loaded; a missing target has no
    // known body, so its outbound links are unknowable.
    let reached = crate::core::bundle::bfs(start, depth, |node| {
        if bundle.contains(node) {
            bundle.outbound(node)
        } else {
            Vec::new()
        }
    });

    let mut included: Vec<(ConceptId, usize)> = Vec::new();
    let mut missing: Vec<ConceptId> = Vec::new();
    // The root leads the included list at distance 0; `bfs` returns the rest
    // already sorted by (distance, id), so the partition stays ordered.
    if bundle.contains(start) {
        included.push((start.clone(), 0));
    } else {
        missing.push(start.clone());
    }
    for (id, distance) in reached {
        if bundle.contains(&id) {
            included.push((id, distance));
        } else {
            missing.push(id);
        }
    }
    missing.sort_unstable();
    (included, missing)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A small bundle:  wau → orders → customers,  orders → ghost (broken).
    fn fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("tables")).unwrap();
        fs::create_dir_all(root.join("metrics")).unwrap();
        fs::write(
            root.join("tables/orders.md"),
            "---\ntype: Table\ntitle: Orders\n---\n# Schema\n\nlinks \
             [customers](/tables/customers.md) and [ghost](/tables/ghost.md)\n",
        )
        .unwrap();
        fs::write(
            root.join("tables/customers.md"),
            "---\ntype: Table\n---\nno outbound links\n",
        )
        .unwrap();
        fs::write(
            root.join("metrics/wau.md"),
            "---\ntype: Metric\ntitle: WAU\n---\nbuilds on [orders](/tables/orders.md)\n",
        )
        .unwrap();
        dir
    }

    fn id(s: &str) -> ConceptId {
        ConceptId::from_relative_path(Path::new(&format!("{s}.md"))).unwrap()
    }

    fn ids<'a>(out: &'a ContextOutput<'a>) -> Vec<&'a str> {
        out.concepts.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn depth_one_includes_root_and_direct_outbound() {
        let dir = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        let out = ContextOutput::build(&bundle, &id("metrics/wau"), 1);
        assert!(out.exists);
        // root first, then the one direct outbound concept.
        assert_eq!(ids(&out), vec!["metrics/wau", "tables/orders"]);
    }

    #[test]
    fn depth_two_follows_outbound_transitively() {
        let dir = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        let out = ContextOutput::build(&bundle, &id("metrics/wau"), 2);
        // wau (0) → orders (1) → customers (2); ghost is broken, not inlined.
        assert_eq!(
            ids(&out),
            vec!["metrics/wau", "tables/orders", "tables/customers"]
        );
        let distance = |s: &str| {
            out.concepts
                .iter()
                .find(|c| c.id.as_str() == s)
                .unwrap()
                .distance
        };
        assert_eq!(distance("metrics/wau"), 0);
        assert_eq!(distance("tables/orders"), 1);
        assert_eq!(distance("tables/customers"), 2);
    }

    #[test]
    fn broken_outbound_link_is_reported_as_missing() {
        let dir = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        let out = ContextOutput::build(&bundle, &id("tables/orders"), 1);
        assert_eq!(out.missing, vec!["tables/ghost"]);
        assert!(!ids(&out).contains(&"tables/ghost"));
    }

    #[test]
    fn missing_root_yields_empty_slice() {
        let dir = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        let out = ContextOutput::build(&bundle, &id("tables/ghost"), 2);
        assert!(!out.exists);
        assert!(out.concepts.is_empty());
        assert_eq!(out.missing, vec!["tables/ghost"]);
    }

    #[test]
    fn does_not_revisit_in_a_cycle() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("a.md"), "---\ntype: T\n---\n[b](/b.md)\n").unwrap();
        fs::write(root.join("b.md"), "---\ntype: T\n---\n[a](/a.md)\n").unwrap();
        let bundle = Bundle::load(root).unwrap();
        let out = ContextOutput::build(&bundle, &id("a"), 5);
        assert_eq!(ids(&out), vec!["a", "b"]);
    }

    #[test]
    fn markdown_blob_is_self_contained() {
        let dir = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        let out = ContextOutput::build(&bundle, &id("metrics/wau"), 1);
        insta::assert_snapshot!(out.render_markdown().unwrap(), @r"
        # Context: metrics/wau (depth 1)

        === metrics/wau ===
        ---
        type: Metric
        title: WAU
        ---
        builds on [orders](/tables/orders.md)

        === tables/orders (depth 1) ===
        ---
        type: Table
        title: Orders
        ---
        # Schema

        links [customers](/tables/customers.md) and [ghost](/tables/ghost.md)
        ");
    }

    #[test]
    fn json_contract_is_stable() {
        let dir = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        let out = ContextOutput::build(&bundle, &id("tables/orders"), 1);
        let json = serde_json::to_string_pretty(&out).unwrap();
        insta::assert_snapshot!(json, @r##"
        {
          "root": "tables/orders",
          "exists": true,
          "depth": 1,
          "concepts": [
            {
              "id": "tables/orders",
              "distance": 0,
              "frontmatter": {
                "type": "Table",
                "title": "Orders"
              },
              "body": "# Schema\n\nlinks [customers](/tables/customers.md) and [ghost](/tables/ghost.md)\n"
            },
            {
              "id": "tables/customers",
              "distance": 1,
              "frontmatter": {
                "type": "Table"
              },
              "body": "no outbound links\n"
            }
          ],
          "missing": [
            "tables/ghost"
          ]
        }
        "##);
    }
}
