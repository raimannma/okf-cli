//! Integration tests over the vendored reference bundles and the hand-crafted
//! edge-case bundle. These exercise [`okf::core::Bundle`] end to end on real and
//! adversarial inputs.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use okf::core::{Bundle, ConceptId};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn id(s: &str) -> ConceptId {
    ConceptId::from_relative_path(Path::new(&format!("{s}.md"))).unwrap()
}

#[test]
fn reference_bundles_load_permissively() {
    // (bundle, expected concept count). Reserved index.md/log.md files are
    // excluded, so these are below the raw .md file counts.
    let cases = [
        ("reference/ga4", 11),
        ("reference/stackoverflow", 49),
        ("reference/crypto_bitcoin", 5),
    ];

    for (name, expected) in cases {
        let bundle = Bundle::load(&fixture(name)).unwrap();
        assert_eq!(bundle.len(), expected, "concept count for {name}");
        assert!(
            bundle.parse_errors().is_empty(),
            "{name} should load without parse errors, got {:?}",
            bundle.parse_errors()
        );
        // Every loaded concept satisfies the §9 minimum: a non-empty `type`.
        for concept in bundle.concepts() {
            assert!(
                concept.frontmatter.type_().is_some(),
                "{}/{} is missing a type",
                name,
                concept.id
            );
        }
    }
}

#[test]
fn edge_case_bundle_is_permissive() {
    let bundle = Bundle::load(&fixture("edge_cases")).unwrap();

    // The malformed file (no frontmatter) is collected, not fatal.
    let errored: Vec<_> = bundle
        .parse_errors()
        .iter()
        .map(|e| e.path.to_string_lossy().into_owned())
        .collect();
    assert_eq!(errored, vec!["malformed.md"]);

    // Reserved files are not concepts; the unknown `type` value is tolerated.
    assert!(!bundle.contains(&id("index")));
    assert!(!bundle.contains(&id("log")));
    assert!(bundle.contains(&id("unknown_type")));

    // A broken link is retained as a backlink edge to a non-existent concept.
    let ghost = id("tables/ghost");
    assert!(!bundle.contains(&ghost));
    assert_eq!(
        bundle.backlinks(&ghost),
        std::slice::from_ref(&id("tables/orders"))
    );

    // Both link forms resolve into the graph: orders links customers with an
    // absolute link; customers and wau link orders with relative links.
    assert_eq!(
        bundle.backlinks(&id("tables/customers")),
        std::slice::from_ref(&id("tables/orders"))
    );
    assert_eq!(
        bundle.backlinks(&id("tables/orders")),
        &[id("metrics/wau"), id("tables/customers")]
    );
}

#[test]
fn ga4_concept_ids_are_stable() {
    let bundle = Bundle::load(&fixture("reference/ga4")).unwrap();
    let ids: Vec<&str> = bundle.concepts().map(|c| c.id.as_str()).collect();
    insta::assert_debug_snapshot!(ids);
}
