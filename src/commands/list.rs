//! `list` subcommand: the cheap survey step.
//!
//! Enumerates a bundle's concepts with their frontmatter only — never bodies —
//! so an agent can see what exists before paying to read anything.

use std::path::Path;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{Date, PrimitiveDateTime, Time};

use crate::core::{Bundle, Concept};
use crate::error::{Error, Result};
use crate::output::OutputMode;

/// The raw filter flags from the `list` subcommand, before validation.
#[derive(Debug)]
pub(crate) struct ListArgs {
    /// Accepted `type` values; empty means "any type". A concept matches if its
    /// type equals any entry.
    pub types: Vec<String>,
    /// Required tags; empty means "no tag filter". A concept matches only if it
    /// carries every entry.
    pub tags: Vec<String>,
    /// Required ID prefix, if any.
    pub path_prefix: Option<String>,
    /// Lower time bound (inclusive) as a raw, unparsed string, if any.
    pub modified_since: Option<String>,
}

/// List the concepts in the bundle rooted at `path`, applying `args` as filters.
///
/// # Errors
///
/// Returns an error if `path` is not a readable bundle directory, or if
/// `args.modified_since` is not a recognized date or datetime. Individual
/// malformed files do not fail the command; they are reported as warnings.
pub(crate) fn run(path: &Path, args: ListArgs, mode: OutputMode) -> Result<()> {
    let filter = ListFilter::from_args(args)?;
    let bundle = Bundle::load(path)?;
    let data = ListData::from_bundle(&bundle, &filter);
    let warnings = crate::output::bundle_warnings(&bundle);

    mode.emit(&data, &data.render_human(), &warnings);
    Ok(())
}

/// The `data` payload of the `list` JSON envelope.
#[derive(Debug, serde::Serialize)]
struct ListData {
    /// How many concepts matched the filters.
    total: usize,
    concepts: Vec<ConceptSummary>,
}

/// One concept's surveyable frontmatter (no body).
#[derive(Debug, serde::Serialize)]
struct ConceptSummary {
    id: String,
    #[serde(rename = "type")]
    type_: Option<String>,
    title: Option<String>,
    description: Option<String>,
    tags: Vec<String>,
}

/// Validated `list` filters. A concept is listed only if it satisfies every
/// active dimension; the dimensions AND together.
#[derive(Debug, Default)]
struct ListFilter {
    types: Vec<String>,
    tags: Vec<String>,
    path_prefix: Option<String>,
    modified_since: Option<OffsetDateTime>,
}

impl ListFilter {
    /// Validate raw flags, parsing the `--modified-since` value if present.
    fn from_args(args: ListArgs) -> Result<Self> {
        let modified_since = match args.modified_since {
            Some(raw) => Some(parse_when(&raw).ok_or(Error::InvalidModifiedSince { value: raw })?),
            None => None,
        };
        Ok(Self {
            types: args.types,
            tags: args.tags,
            path_prefix: args.path_prefix,
            modified_since,
        })
    }

    /// Whether `concept` passes every active filter dimension.
    fn matches(&self, concept: &Concept) -> bool {
        if !self.types.is_empty() {
            let matches_type = concept
                .frontmatter
                .type_()
                .is_some_and(|t| self.types.iter().any(|want| want == t));
            if !matches_type {
                return false;
            }
        }

        if !self.tags.is_empty() {
            let have = concept.frontmatter.tags();
            let has_all = self
                .tags
                .iter()
                .all(|want| have.iter().any(|tag| tag == want));
            if !has_all {
                return false;
            }
        }

        if let Some(prefix) = &self.path_prefix
            && !concept.id.as_str().starts_with(prefix.as_str())
        {
            return false;
        }

        if let Some(since) = self.modified_since {
            match concept_modified(concept) {
                Some(when) if when >= since => {}
                _ => return false,
            }
        }

        true
    }
}

/// A concept's effective "modified" time: its `timestamp` frontmatter field if
/// present and parseable, otherwise the source file's last-modified time.
fn concept_modified(concept: &Concept) -> Option<OffsetDateTime> {
    if let Some(parsed) = concept
        .frontmatter
        .get_str("timestamp")
        .and_then(parse_when)
    {
        return Some(parsed);
    }
    concept.modified.map(OffsetDateTime::from)
}

/// Parse an RFC 3339 datetime or a bare `YYYY-MM-DD` date (midnight UTC).
fn parse_when(value: &str) -> Option<OffsetDateTime> {
    let value = value.trim();
    if let Ok(datetime) = OffsetDateTime::parse(value, &Rfc3339) {
        return Some(datetime);
    }
    let date = Date::parse(value, format_description!("[year]-[month]-[day]")).ok()?;
    Some(PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_utc())
}

impl ListData {
    fn from_bundle(bundle: &Bundle, filter: &ListFilter) -> Self {
        let concepts: Vec<ConceptSummary> = bundle
            .concepts()
            .filter(|c| filter.matches(c))
            .map(|c| ConceptSummary {
                id: c.id.to_string(),
                type_: c.frontmatter.type_().map(str::to_owned),
                title: c.frontmatter.title().map(str::to_owned),
                description: c.frontmatter.description().map(str::to_owned),
                tags: c
                    .frontmatter
                    .tags()
                    .iter()
                    .map(|t| (*t).to_owned())
                    .collect(),
            })
            .collect();

        Self {
            total: concepts.len(),
            concepts,
        }
    }

    /// Render the listing for humans: a one-line summary header, then one
    /// `id  [type]  label` row per concept with the id and type columns aligned
    /// so an agent (or a person) can scan them.
    fn render_human(&self) -> String {
        use std::fmt::Write as _;

        if self.concepts.is_empty() {
            return "no concepts match\n".to_owned();
        }

        let type_count = self
            .concepts
            .iter()
            .filter_map(|c| c.type_.as_deref())
            .collect::<std::collections::BTreeSet<_>>()
            .len();

        let id_width = self
            .concepts
            .iter()
            .map(|c| c.id.chars().count())
            .max()
            .unwrap_or(0);
        let type_width = self
            .concepts
            .iter()
            .map(|c| display_type(c).chars().count() + 2)
            .max()
            .unwrap_or(0);

        let mut out = String::new();
        let _ = writeln!(
            out,
            "{} {} · {type_count} {}",
            self.total,
            plural(self.total, "concept"),
            plural(type_count, "type"),
        );
        out.push('\n');
        for concept in &self.concepts {
            let bracketed = format!("[{}]", display_type(concept));
            match concept.title.as_deref().or(concept.description.as_deref()) {
                Some(label) => {
                    let _ = writeln!(
                        out,
                        "{:<id_width$}  {:<type_width$}  {label}",
                        concept.id, bracketed
                    );
                }
                None => {
                    let _ = writeln!(out, "{:<id_width$}  {bracketed}", concept.id);
                }
            }
        }
        out
    }
}

/// The concept's `type`, or a `no type` placeholder for the JSON-null case.
fn display_type(concept: &ConceptSummary) -> &str {
    concept.type_.as_deref().unwrap_or("no type")
}

/// `"1 concept"` / `"3 concepts"` — naive English pluralization for the header.
fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fixture() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("tables")).unwrap();
        fs::write(
            root.join("tables/orders.md"),
            "---\ntype: BigQuery Table\ntitle: Customer Orders\ntags: [sales, orders]\n---\nbody\n",
        )
        .unwrap();
        fs::write(
            root.join("metrics.md"),
            "---\ntype: Metric\n---\nno title here\n",
        )
        .unwrap();
        fs::write(root.join("broken.md"), "no frontmatter\n").unwrap();
        dir
    }

    fn ids(output: &ListData) -> Vec<&str> {
        output.concepts.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn human_listing_is_stable() {
        let dir = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        let output = ListData::from_bundle(&bundle, &ListFilter::default());
        insta::assert_snapshot!(output.render_human(), @r"
        2 concepts · 2 types

        metrics        [Metric]
        tables/orders  [BigQuery Table]  Customer Orders
        ");
    }

    #[test]
    fn empty_listing_reports_no_match() {
        let dir = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        let filter = ListFilter::from_args(ListArgs {
            types: vec!["Nonexistent".to_owned()],
            ..args()
        })
        .unwrap();
        let output = ListData::from_bundle(&bundle, &filter);
        assert_eq!(output.render_human(), "no concepts match\n");
    }

    #[test]
    fn json_contract_is_stable() {
        let dir = fixture();
        let bundle = Bundle::load(dir.path()).unwrap();
        let output = ListData::from_bundle(&bundle, &ListFilter::default());
        insta::assert_json_snapshot!(output, @r#"
        {
          "total": 2,
          "concepts": [
            {
              "id": "metrics",
              "type": "Metric",
              "title": null,
              "description": null,
              "tags": []
            },
            {
              "id": "tables/orders",
              "type": "BigQuery Table",
              "title": "Customer Orders",
              "description": null,
              "tags": [
                "sales",
                "orders"
              ]
            }
          ]
        }
        "#);
    }

    fn filtered(dir: &TempDir, args: ListArgs) -> Vec<String> {
        let bundle = Bundle::load(dir.path()).unwrap();
        let filter = ListFilter::from_args(args).unwrap();
        let output = ListData::from_bundle(&bundle, &filter);
        ids(&output).into_iter().map(str::to_owned).collect()
    }

    fn args() -> ListArgs {
        ListArgs {
            types: Vec::new(),
            tags: Vec::new(),
            path_prefix: None,
            modified_since: None,
        }
    }

    #[test]
    fn type_filter_matches_any_listed_type() {
        let dir = fixture();
        let only_metric = filtered(
            &dir,
            ListArgs {
                types: vec!["Metric".to_owned()],
                ..args()
            },
        );
        assert_eq!(only_metric, vec!["metrics"]);

        let either = filtered(
            &dir,
            ListArgs {
                types: vec!["Metric".to_owned(), "BigQuery Table".to_owned()],
                ..args()
            },
        );
        assert_eq!(either, vec!["metrics", "tables/orders"]);
    }

    #[test]
    fn tag_filter_requires_all_tags() {
        let dir = fixture();
        let both = filtered(
            &dir,
            ListArgs {
                tags: vec!["sales".to_owned(), "orders".to_owned()],
                ..args()
            },
        );
        assert_eq!(both, vec!["tables/orders"]);

        let missing = filtered(
            &dir,
            ListArgs {
                tags: vec!["sales".to_owned(), "absent".to_owned()],
                ..args()
            },
        );
        assert!(missing.is_empty());
    }

    #[test]
    fn path_prefix_filters_by_id() {
        let dir = fixture();
        let tables = filtered(
            &dir,
            ListArgs {
                path_prefix: Some("tables/".to_owned()),
                ..args()
            },
        );
        assert_eq!(tables, vec!["tables/orders"]);
    }

    #[test]
    fn filters_combine_with_and() {
        let dir = fixture();
        let none = filtered(
            &dir,
            ListArgs {
                types: vec!["Metric".to_owned()],
                path_prefix: Some("tables/".to_owned()),
                ..args()
            },
        );
        assert!(none.is_empty());
    }

    #[test]
    fn modified_since_uses_frontmatter_timestamp() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(
            root.join("old.md"),
            "---\ntype: Metric\ntimestamp: 2024-01-01T00:00:00Z\n---\nold\n",
        )
        .unwrap();
        fs::write(
            root.join("new.md"),
            "---\ntype: Metric\ntimestamp: 2026-06-01T00:00:00Z\n---\nnew\n",
        )
        .unwrap();

        let recent = filtered(
            &dir,
            ListArgs {
                modified_since: Some("2026-01-01".to_owned()),
                ..args()
            },
        );
        assert_eq!(recent, vec!["new"]);
    }

    #[test]
    fn modified_since_falls_back_to_file_mtime() {
        let dir = TempDir::new().unwrap();
        // No `timestamp` frontmatter, so the file's mtime (just now) decides.
        fs::write(dir.path().join("c.md"), "---\ntype: Metric\n---\nbody\n").unwrap();

        let included = filtered(
            &dir,
            ListArgs {
                modified_since: Some("2000-01-01".to_owned()),
                ..args()
            },
        );
        assert_eq!(included, vec!["c"]);

        let excluded = filtered(
            &dir,
            ListArgs {
                modified_since: Some("2999-01-01".to_owned()),
                ..args()
            },
        );
        assert!(excluded.is_empty());
    }

    #[test]
    fn invalid_modified_since_is_an_error() {
        let err = ListFilter::from_args(ListArgs {
            modified_since: Some("not-a-date".to_owned()),
            ..args()
        })
        .unwrap_err();
        assert!(matches!(err, Error::InvalidModifiedSince { .. }));
    }

    #[test]
    fn parse_when_accepts_date_and_datetime() {
        assert!(parse_when("2026-06-20").is_some());
        assert!(parse_when("2026-06-20T12:00:00Z").is_some());
        assert!(parse_when("nonsense").is_none());
    }
}
