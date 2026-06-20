//! `get` subcommand: fetch one concept, with partial reads.
//!
//! Drill-down after the cheap `list` survey: return a whole concept, or just its
//! frontmatter, body, or a single section — so an agent pays only for the slice
//! it asked for.

use std::path::Path;
use std::process::ExitCode;

use crate::commands::mutate::lookup_id;
use crate::core::{Bundle, Concept, Frontmatter};
use crate::error::{Error, Result};
use crate::output::OutputMode;

/// Which slice of a concept to return. The partial modes are mutually exclusive
/// (enforced by clap); `Full` returns frontmatter and body together.
#[derive(Debug)]
pub(crate) enum GetScope {
    Full,
    FrontmatterOnly,
    BodyOnly,
    Section(String),
}

/// Fetch each concept in `concept_ids` from the bundle at `path` and render `scope`.
///
/// A bad id (malformed, not found, or — under `--section` — missing the heading)
/// does not abort the command: the remaining concepts are still returned, and the
/// failures are reported. Text output emits a single concept raw and several under
/// `=== <id> ===` headers, with one `warning:` line per failure on stderr; JSON
/// output carries the resolved `concepts` and the failed ids in `data.errors`.
///
/// Returns the process exit code: success when every requested id resolved, or
/// the most severe per-id failure's [`Error::exit_code`] when some did not — so a
/// `get` that couldn't serve a concept fails the command even though it still
/// prints the concepts that did resolve.
///
/// # Errors
///
/// Returns [`Error::BundleNotADirectory`] if `path` is not a bundle. Per-id
/// failures are collected and reported (and drive the exit code), not returned.
pub(crate) fn run(
    path: &Path,
    concept_ids: &[String],
    scope: &GetScope,
    mode: OutputMode,
) -> Result<ExitCode> {
    let bundle = Bundle::load(path)?;
    let warnings = crate::output::bundle_warnings(&bundle);

    // Resolve every requested id; a bad id becomes a per-id error, not an abort.
    let mut concepts = Vec::new();
    let mut failures = Failures::default();
    for concept_id in concept_ids {
        match resolve(&bundle, concept_id) {
            Ok(concept) => concepts.push(concept),
            Err(err) => failures.record(concept_id.clone(), &err),
        }
    }

    // Render each resolved concept to JSON and text under one error path, so a
    // section-not-found shows up identically in both formats.
    let mut items: Vec<GetJson> = Vec::new();
    let mut slices: Vec<(String, String)> = Vec::new();
    for concept in &concepts {
        match (concept_json(concept, scope), render_slice(concept, scope)) {
            (Ok(json), Ok(text)) => {
                items.push(json);
                slices.push((concept.id.to_string(), text));
            }
            (Err(err), _) | (_, Err(err)) => failures.record(concept.id.to_string(), &err),
        }
    }

    let text = assemble(&slices);
    let failure_code = failures.code;
    let data = GetOutput {
        concepts: items,
        errors: failures.errors,
    };

    if mode.is_structured() {
        mode.emit(&data, &text, &warnings);
    } else {
        // Per-id failures are part of the answer the user asked for, so in text
        // mode they go to stderr alongside any bundle-load warnings.
        let mut text_warnings = warnings;
        for e in &data.errors {
            text_warnings.push(format!("{}: {}", e.id, e.error));
        }
        mode.emit(&data, &text, &text_warnings);
    }
    Ok(ExitCode::from(failure_code))
}

/// Join rendered concept slices for text output: a single concept is emitted
/// raw (clean to pipe), several are separated by `=== <id> ===` headers so the
/// boundary between concepts is unambiguous.
fn assemble(slices: &[(String, String)]) -> String {
    use std::fmt::Write as _;

    match slices {
        [] => String::new(),
        [(_, text)] => text.clone(),
        many => {
            let mut out = String::new();
            for (i, (id, text)) in many.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                let _ = write!(out, "=== {id} ===\n{text}");
                if !text.ends_with('\n') {
                    out.push('\n');
                }
            }
            out
        }
    }
}

/// Resolve one concept id against the bundle, or a typed error explaining why it
/// could not be served.
fn resolve<'a>(bundle: &'a Bundle, concept_id: &str) -> Result<&'a Concept> {
    let id = lookup_id(concept_id)?;
    bundle
        .get(&id)
        .ok_or_else(|| Error::ConceptNotFound { id: id.to_string() })
}

/// Resolve the body text for a `--section` request, or a not-found error.
fn section_text<'a>(concept: &'a Concept, heading: &str) -> Result<&'a str> {
    concept
        .section(heading)
        .ok_or_else(|| Error::SectionNotFound {
            section: heading.to_owned(),
            id: concept.id.to_string(),
        })
}

/// Render the YAML rendering of a concept's frontmatter, mapping failures to a
/// typed error.
fn frontmatter_yaml(concept: &Concept) -> Result<String> {
    concept
        .frontmatter
        .to_yaml()
        .map_err(|source| Error::RenderFrontmatter {
            id: concept.id.to_string(),
            source,
        })
}

/// The text rendering of one concept's requested `scope` (no id header — the
/// caller adds delimiters when more than one concept is returned).
fn render_slice(concept: &Concept, scope: &GetScope) -> Result<String> {
    Ok(match scope {
        GetScope::Full => format!("---\n{}---\n{}", frontmatter_yaml(concept)?, concept.body),
        GetScope::FrontmatterOnly => frontmatter_yaml(concept)?,
        GetScope::BodyOnly => concept.body.clone(),
        GetScope::Section(heading) => section_text(concept, heading)?.to_owned(),
    })
}

/// One concept id that could not be served, with a user-facing reason. Carried
/// in the `data.errors` array (and, in text mode, echoed to stderr).
#[derive(serde::Serialize)]
struct GetError {
    id: String,
    error: String,
}

impl GetError {
    fn new(id: String, err: &Error) -> Self {
        Self {
            id,
            error: err.to_string(),
        }
    }
}

/// Accumulates per-id failures and the most severe exit code among them, so a
/// `get` that couldn't serve some ids still reports the rest but fails the run.
#[derive(Default)]
struct Failures {
    errors: Vec<GetError>,
    code: u8,
}

impl Failures {
    fn record(&mut self, id: String, err: &Error) {
        self.code = self.code.max(err.exit_code());
        self.errors.push(GetError::new(id, err));
    }
}

/// The `data` payload of the `get` JSON envelope: the concepts that resolved,
/// plus the ids that did not.
#[derive(serde::Serialize)]
struct GetOutput<'a> {
    concepts: Vec<GetJson<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errors: Vec<GetError>,
}

/// One concept's slice in the JSON output. Always carries the `id`; the other
/// keys depend on the requested slice. Serialized directly (no intermediate
/// `serde_json::Value`) so frontmatter key order is preserved.
#[derive(serde::Serialize)]
struct GetJson<'a> {
    id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    section: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frontmatter: Option<&'a Frontmatter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
}

/// Build the [`GetJson`] view of a single concept under `scope`.
fn concept_json<'a>(concept: &'a Concept, scope: &'a GetScope) -> Result<GetJson<'a>> {
    let id = concept.id.as_str();
    Ok(match scope {
        GetScope::Full => GetJson {
            id,
            section: None,
            frontmatter: Some(&concept.frontmatter),
            body: Some(&concept.body),
        },
        GetScope::FrontmatterOnly => GetJson {
            id,
            section: None,
            frontmatter: Some(&concept.frontmatter),
            body: None,
        },
        GetScope::BodyOnly => GetJson {
            id,
            section: None,
            frontmatter: None,
            body: Some(&concept.body),
        },
        GetScope::Section(heading) => GetJson {
            id,
            section: Some(heading),
            frontmatter: None,
            body: Some(section_text(concept, heading)?),
        },
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::core::ConceptId;

    fn concept() -> Concept {
        let content = "---\ntype: Table\ntitle: Orders\ntags: [sales]\n---\n\
            # Overview\n\nintro\n\n# Schema\n\n- id\n- total\n";
        let id = ConceptId::from_relative_path(Path::new("tables/orders.md")).unwrap();
        Concept::parse(id, content).unwrap()
    }

    fn other_concept() -> Concept {
        let content = "---\ntype: Table\ntitle: Customers\n---\n# Body\n\nrows\n";
        let id = ConceptId::from_relative_path(Path::new("tables/customers.md")).unwrap();
        Concept::parse(id, content).unwrap()
    }

    /// Build the `data` payload exactly as `run` does, then pretty-print it — the
    /// stable inner JSON the envelope wraps.
    fn render_json(
        concepts: &[&Concept],
        errors: Vec<GetError>,
        scope: &GetScope,
    ) -> Result<String> {
        let mut items = Vec::new();
        let mut errors = errors;
        for concept in concepts {
            match concept_json(concept, scope) {
                Ok(json) => items.push(json),
                Err(err) => errors.push(GetError::new(concept.id.to_string(), &err)),
            }
        }
        let output = GetOutput {
            concepts: items,
            errors,
        };
        serde_json::to_string_pretty(&output).map_err(Error::Json)
    }

    #[test]
    fn full_human_reconstructs_document() {
        let c = concept();
        insta::assert_snapshot!(render_slice(&c, &GetScope::Full).unwrap(), @r"
        ---
        type: Table
        title: Orders
        tags:
        - sales
        ---
        # Overview

        intro

        # Schema

        - id
        - total
        ");
    }

    #[test]
    fn frontmatter_only_human_preserves_key_order() {
        let c = concept();
        insta::assert_snapshot!(render_slice(&c, &GetScope::FrontmatterOnly).unwrap(), @r"
        type: Table
        title: Orders
        tags:
        - sales
        ");
    }

    #[test]
    fn body_only_human_is_just_the_body() {
        let c = concept();
        let body = render_slice(&c, &GetScope::BodyOnly).unwrap();
        assert!(body.starts_with("# Overview"));
        assert!(!body.contains("type: Table"));
    }

    #[test]
    fn section_human_returns_single_heading() {
        let c = concept();
        let schema = render_slice(&c, &GetScope::Section("# Schema".to_owned())).unwrap();
        assert_eq!(schema, "# Schema\n\n- id\n- total\n");
    }

    #[test]
    fn missing_section_is_an_error() {
        let c = concept();
        let err = render_slice(&c, &GetScope::Section("# Missing".to_owned())).unwrap_err();
        assert!(matches!(err, Error::SectionNotFound { .. }));
    }

    #[test]
    fn full_json_contract_is_stable() {
        let c = concept();
        let json = render_json(&[&c], vec![], &GetScope::Full).unwrap();
        insta::assert_snapshot!(json, @r##"
        {
          "concepts": [
            {
              "id": "tables/orders",
              "frontmatter": {
                "type": "Table",
                "title": "Orders",
                "tags": [
                  "sales"
                ]
              },
              "body": "# Overview\n\nintro\n\n# Schema\n\n- id\n- total\n"
            }
          ]
        }
        "##);
    }

    #[test]
    fn section_json_carries_heading_and_text() {
        let c = concept();
        let json = render_json(&[&c], vec![], &GetScope::Section("# Schema".to_owned())).unwrap();
        insta::assert_snapshot!(json, @r##"
        {
          "concepts": [
            {
              "id": "tables/orders",
              "section": "# Schema",
              "body": "# Schema\n\n- id\n- total\n"
            }
          ]
        }
        "##);
    }

    #[test]
    fn multiple_concepts_json_lists_each() {
        let a = concept();
        let b = other_concept();
        let json = render_json(&[&a, &b], vec![], &GetScope::FrontmatterOnly).unwrap();
        insta::assert_snapshot!(json, @r##"
        {
          "concepts": [
            {
              "id": "tables/orders",
              "frontmatter": {
                "type": "Table",
                "title": "Orders",
                "tags": [
                  "sales"
                ]
              }
            },
            {
              "id": "tables/customers",
              "frontmatter": {
                "type": "Table",
                "title": "Customers"
              }
            }
          ]
        }
        "##);
    }

    #[test]
    fn json_reports_resolution_and_section_errors() {
        let c = concept();
        let resolution = GetError {
            id: "tables/ghost".to_owned(),
            error: "concept `tables/ghost` not found in bundle".to_owned(),
        };
        let json = render_json(
            &[&c],
            vec![resolution],
            &GetScope::Section("# Missing".to_owned()),
        )
        .unwrap();
        insta::assert_snapshot!(json, @r##"
        {
          "concepts": [],
          "errors": [
            {
              "id": "tables/ghost",
              "error": "concept `tables/ghost` not found in bundle"
            },
            {
              "id": "tables/orders",
              "error": "section `# Missing` not found in concept `tables/orders`"
            }
          ]
        }
        "##);
    }

    #[test]
    fn single_concept_text_has_no_id_header() {
        // One concept pipes raw: no `=== id ===` line to corrupt the document.
        let c = other_concept();
        let slice = render_slice(&c, &GetScope::BodyOnly).unwrap();
        let assembled = assemble(&[("tables/customers".to_owned(), slice)]);
        assert_eq!(assembled, "# Body\n\nrows\n");
    }

    #[test]
    fn multiple_concepts_text_is_delimited_by_id() {
        let a = concept();
        let b = other_concept();
        let slices = [&a, &b]
            .iter()
            .map(|c| {
                (
                    c.id.to_string(),
                    render_slice(c, &GetScope::BodyOnly).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        insta::assert_snapshot!(assemble(&slices), @r"
        === tables/orders ===
        # Overview

        intro

        # Schema

        - id
        - total

        === tables/customers ===
        # Body

        rows
        ");
    }
}
