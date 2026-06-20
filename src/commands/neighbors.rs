//! `neighbors` subcommand: a concept's graph neighbors, IDs only.
//!
//! The targeted graph query an agent actually wants: what does this concept link
//! to (`outbound`), and what links to it (`backlinks`, "cited by") — optionally
//! transitively via `--depth`. It returns IDs plus whether each target exists and
//! its shortest distance, never bodies, so the answer stays scoped to the question.
//!
//! Cost is dominated by loading the bundle once (parallelized in [`Bundle::load`]).
//! The traversal itself is a breadth-first walk that only ever touches the
//! reachable neighborhood — it scales with the size of the answer and the
//! requested depth, not with the size of the bundle, so it stays fast even when
//! the folder tree is huge.

use std::path::Path;

use crate::commands::mutate::lookup_id;
use crate::core::{Bundle, ConceptId};
use crate::error::Result;
use crate::output::OutputMode;

/// Resolve `concept_id` against the bundle at `path` and report its neighbors out
/// to `depth` hops in each direction.
///
/// A concept that resolves but is not loaded is **not** an error: broken links
/// are first-class in OKF, so an absent concept can still be cited. Such a
/// concept is reported with `exists: false`, an empty `outbound` (its links are
/// unknowable without its body), and whatever `backlinks` point at it.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] if `concept_id` is not a valid concept id, or
/// [`Error::BundleNotADirectory`] if `path` is not a readable bundle, or
/// [`Error::Json`] if JSON output cannot be serialized.
pub(crate) fn run(path: &Path, concept_id: &str, depth: usize, mode: OutputMode) -> Result<()> {
    let id = lookup_id(concept_id)?;

    let bundle = Bundle::load(path)?;
    let output = NeighborsOutput::build(&bundle, &id, depth);
    let warnings = crate::output::bundle_warnings(&bundle);

    mode.emit(&output, &output.render_human(), &warnings);
    Ok(())
}

/// The stable JSON contract for `okf neighbors`.
#[derive(Debug, serde::Serialize)]
struct NeighborsOutput {
    /// The concept the query was rooted at.
    concept: String,
    /// Whether that concept is actually loaded in the bundle.
    exists: bool,
    /// The maximum hop count traversed in each direction.
    depth: usize,
    /// Concepts reachable by following this concept's links, nearest first.
    outbound: Vec<Neighbor>,
    /// Concepts that reach this one by their links ("cited by"), nearest first.
    backlinks: Vec<Neighbor>,
}

/// One neighbor: which concept, whether it exists, and its shortest distance.
#[derive(Debug, serde::Serialize)]
struct Neighbor {
    id: String,
    exists: bool,
    /// Hops from the root concept (1 = a direct neighbor).
    distance: usize,
}

impl NeighborsOutput {
    fn build(bundle: &Bundle, id: &ConceptId, depth: usize) -> Self {
        let outbound = crate::core::bundle::bfs(id, depth, |node| bundle.outbound(node));
        let backlinks = crate::core::bundle::bfs(id, depth, |node| bundle.backlinks(node).to_vec());
        Self {
            concept: id.to_string(),
            exists: bundle.contains(id),
            depth,
            outbound: to_neighbors(bundle, outbound),
            backlinks: to_neighbors(bundle, backlinks),
        }
    }

    /// Render for humans: the concept, then each direction with its count and a
    /// nearest-first list. Distances are shown only when a deeper traversal can
    /// produce them (`depth > 1`).
    fn render_human(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let _ = writeln!(
            out,
            "{}{}",
            self.concept,
            if self.exists { "" } else { "  (missing)" }
        );

        for (arrow, label, list) in [
            ('→', "outbound", &self.outbound),
            ('←', "backlinks (cited by)", &self.backlinks),
        ] {
            out.push('\n');
            let _ = writeln!(out, "{arrow} {label} ({})", list.len());
            if list.is_empty() {
                out.push_str("  (none)\n");
                continue;
            }
            for n in list {
                let missing = if n.exists { "" } else { "  (missing)" };
                let dist = if self.depth > 1 {
                    format!("  ·depth {}", n.distance)
                } else {
                    String::new()
                };
                let _ = writeln!(out, "  {}{missing}{dist}", n.id);
            }
        }
        out
    }
}

/// Pair each traversed ID with whether the bundle actually loaded it.
fn to_neighbors(bundle: &Bundle, ids: Vec<(ConceptId, usize)>) -> Vec<Neighbor> {
    ids.into_iter()
        .map(|(id, distance)| Neighbor {
            exists: bundle.contains(&id),
            id: id.to_string(),
            distance,
        })
        .collect()
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
            "---\ntype: Table\n---\nlinks [customers](/tables/customers.md) and \
             [ghost](/tables/ghost.md)\n",
        )
        .unwrap();
        fs::write(
            root.join("tables/customers.md"),
            "---\ntype: Table\n---\nno outbound links\n",
        )
        .unwrap();
        fs::write(
            root.join("metrics/wau.md"),
            "---\ntype: Metric\n---\nbuilds on [orders](/tables/orders.md)\n",
        )
        .unwrap();
        dir
    }

    fn build(dir: &TempDir, concept: &str, depth: usize) -> NeighborsOutput {
        let bundle = Bundle::load(dir.path()).unwrap();
        let id = ConceptId::from_relative_path(Path::new(&format!("{concept}.md"))).unwrap();
        NeighborsOutput::build(&bundle, &id, depth)
    }

    fn ids(list: &[Neighbor]) -> Vec<&str> {
        list.iter().map(|n| n.id.as_str()).collect()
    }

    #[test]
    fn depth_one_returns_direct_neighbors_both_directions() {
        let dir = fixture();
        let out = build(&dir, "tables/orders", 1);
        assert!(out.exists);
        assert_eq!(ids(&out.outbound), vec!["tables/customers", "tables/ghost"]);
        assert_eq!(ids(&out.backlinks), vec!["metrics/wau"]);
    }

    #[test]
    fn broken_link_target_is_marked_missing() {
        let dir = fixture();
        let out = build(&dir, "tables/orders", 1);
        let ghost = out
            .outbound
            .iter()
            .find(|n| n.id == "tables/ghost")
            .unwrap();
        assert!(!ghost.exists);
        let customers = out
            .outbound
            .iter()
            .find(|n| n.id == "tables/customers")
            .unwrap();
        assert!(customers.exists);
    }

    #[test]
    fn depth_two_follows_links_transitively_with_distance() {
        let dir = fixture();
        // From wau: depth 1 → orders; depth 2 → orders' targets (customers, ghost).
        let out = build(&dir, "metrics/wau", 2);
        assert_eq!(
            ids(&out.outbound),
            vec!["tables/orders", "tables/customers", "tables/ghost"]
        );
        let distance = |id: &str| out.outbound.iter().find(|n| n.id == id).unwrap().distance;
        assert_eq!(distance("tables/orders"), 1);
        assert_eq!(distance("tables/customers"), 2);
        assert_eq!(distance("tables/ghost"), 2);
    }

    #[test]
    fn shortest_distance_wins_when_reachable_two_ways() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        // a → b, a → c, b → c.  c is reachable from a at distance 1 and 2;
        // the traversal must record the shorter one.
        fs::write(
            root.join("a.md"),
            "---\ntype: T\n---\n[b](/b.md) [c](/c.md)\n",
        )
        .unwrap();
        fs::write(root.join("b.md"), "---\ntype: T\n---\n[c](/c.md)\n").unwrap();
        fs::write(root.join("c.md"), "---\ntype: T\n---\nleaf\n").unwrap();

        let out = build(&dir, "a", 3);
        let c = out.outbound.iter().find(|n| n.id == "c").unwrap();
        assert_eq!(c.distance, 1);
    }

    #[test]
    fn nonexistent_concept_still_reports_backlinks() {
        let dir = fixture();
        // `tables/ghost` is never loaded, but `tables/orders` cites it.
        let out = build(&dir, "tables/ghost", 1);
        assert!(!out.exists);
        assert!(out.outbound.is_empty());
        assert_eq!(ids(&out.backlinks), vec!["tables/orders"]);
    }

    #[test]
    fn depth_does_not_revisit_the_root() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        // A cycle a → b → a must not list `a` as its own neighbor.
        fs::write(root.join("a.md"), "---\ntype: T\n---\n[b](/b.md)\n").unwrap();
        fs::write(root.join("b.md"), "---\ntype: T\n---\n[a](/a.md)\n").unwrap();

        let out = build(&dir, "a", 5);
        assert_eq!(ids(&out.outbound), vec!["b"]);
    }

    #[test]
    fn human_output_is_readable() {
        let dir = fixture();
        let out = build(&dir, "tables/orders", 1);
        insta::assert_snapshot!(out.render_human(), @r"
        tables/orders

        → outbound (2)
          tables/customers
          tables/ghost  (missing)

        ← backlinks (cited by) (1)
          metrics/wau
        ");
    }

    #[test]
    fn json_contract_is_stable() {
        let dir = fixture();
        let out = build(&dir, "metrics/wau", 2);
        let json = serde_json::to_string_pretty(&out).unwrap();
        insta::assert_snapshot!(json, @r#"
        {
          "concept": "metrics/wau",
          "exists": true,
          "depth": 2,
          "outbound": [
            {
              "id": "tables/orders",
              "exists": true,
              "distance": 1
            },
            {
              "id": "tables/customers",
              "exists": true,
              "distance": 2
            },
            {
              "id": "tables/ghost",
              "exists": false,
              "distance": 2
            }
          ],
          "backlinks": []
        }
        "#);
    }
}
