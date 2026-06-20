//! Generating the reserved `index.md` directory listings (SPEC §6).
//!
//! An `index.md` is a pure function of the concepts beneath its directory: each
//! directory lists its immediate subdirectories under a `# Subdirectories`
//! heading and its direct concepts grouped under a `# <type>` heading, every
//! entry a markdown link plus the concept's `description`. Because the listing is
//! derived, it can be regenerated at any time and kept in sync by the mutating
//! commands.
//!
//! Two pieces of an existing `index.md` are *not* derivable and are preserved on
//! regeneration: a bundle-root `index.md` frontmatter block (the only place
//! frontmatter is allowed, SPEC §11) and any human-written description on a
//! subdirectory entry. Both are read back from the prior file via the `existing`
//! callback.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::core::Concept;
use crate::core::concept_id::ConceptId;
use crate::core::frontmatter;

/// The reserved listing filename generated at every level of the hierarchy.
pub const INDEX_FILENAME: &str = "index.md";

/// The fields of a concept that an index listing draws on — its id (for the link
/// and its directory) plus the three frontmatter fields that shape an entry.
///
/// Mutating commands build the *post-mutation* set of these so a regenerated
/// index reflects an edit that is not yet (or never, under `--dry-run`) on disk.
#[derive(Debug, Clone)]
pub struct ConceptSummary {
    /// The concept's id (bundle-relative path without `.md`).
    pub id: ConceptId,
    /// The `type` frontmatter field, if non-empty — the heading the entry groups under.
    pub type_: Option<String>,
    /// The `title` frontmatter field, if any — the link text (else the filename stem).
    pub title: Option<String>,
    /// The `description` frontmatter field, if any — the entry's trailing blurb.
    pub description: Option<String>,
}

impl ConceptSummary {
    /// Summarize a loaded concept for index generation.
    #[must_use]
    pub fn from_concept(concept: &Concept) -> Self {
        Self::from_frontmatter(concept.id.clone(), &concept.frontmatter)
    }

    /// Summarize a concept from its id and frontmatter, for the mutating commands
    /// that must describe a post-mutation concept not yet (or never, under
    /// `--dry-run`) parsed back off disk.
    #[must_use]
    pub fn from_frontmatter(id: ConceptId, frontmatter: &crate::core::Frontmatter) -> Self {
        Self {
            id,
            type_: frontmatter.type_().map(str::to_owned),
            title: frontmatter.title().map(str::to_owned),
            description: frontmatter.description().map(str::to_owned),
        }
    }
}

/// The desired content of one directory's `index.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedIndex {
    /// The index file's bundle-relative path, e.g. `tables/index.md` or `index.md`.
    pub path: String,
    /// The desired file content.
    pub content: String,
}

/// Plan every `index.md` the bundle should have, given the concepts that should
/// exist and a callback returning the current content of an index file by its
/// bundle-relative path (used to preserve root frontmatter and subdirectory
/// descriptions).
///
/// Returns one [`PlannedIndex`] per directory that (transitively) contains a
/// concept, ordered by path. Directories whose `index.md` is *not* in the result
/// should have it removed — the caller owns that, since planning is pure.
#[must_use]
pub fn plan(
    concepts: &[ConceptSummary],
    existing: &impl Fn(&str) -> Option<String>,
) -> Vec<PlannedIndex> {
    // Every directory that holds a concept, directly or transitively, gets an
    // index. Track each directory's direct concepts as we discover it.
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    let mut direct: BTreeMap<String, Vec<&ConceptSummary>> = BTreeMap::new();
    for concept in concepts {
        let (dir, _) = split_id(concept.id.as_str());
        direct.entry(dir.clone()).or_default().push(concept);
        // Register the directory and every ancestor up to the root.
        let mut ancestor = Some(dir);
        while let Some(d) = ancestor {
            ancestor = (!d.is_empty()).then(|| parent_dir(&d));
            dirs.insert(d);
        }
    }

    // The immediate subdirectories of each directory, derived from the dir set so
    // a directory with only subdirectories (and no direct concepts) still lists them.
    let mut children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for dir in &dirs {
        if dir.is_empty() {
            continue;
        }
        children
            .entry(parent_dir(dir))
            .or_default()
            .insert(last_segment(dir).to_owned());
    }

    dirs.iter()
        .map(|dir| {
            let path = index_path(dir);
            let prior = existing(&path);
            let kept = prior
                .as_deref()
                .map(parse_entry_descriptions)
                .unwrap_or_default();

            let subdirs: Vec<SubdirEntry> = children
                .get(dir)
                .into_iter()
                .flatten()
                .map(|name| SubdirEntry {
                    desc: kept.get(&format!("{name}/{INDEX_FILENAME}")).cloned(),
                    name: name.clone(),
                })
                .collect();

            let groups = concept_groups(direct.get(dir).map(Vec::as_slice).unwrap_or_default());

            // Only the bundle root may carry frontmatter (SPEC §11); preserve it.
            let frontmatter = dir
                .is_empty()
                .then(|| prior.as_deref().and_then(leading_frontmatter))
                .flatten();

            PlannedIndex {
                path,
                content: render(&subdirs, &groups, frontmatter),
            }
        })
        .collect()
}

/// A subdirectory entry: its directory name and any preserved description.
struct SubdirEntry {
    name: String,
    desc: Option<String>,
}

/// A type-headed group of concept entries.
struct ConceptGroup {
    heading: String,
    entries: Vec<Entry>,
}

/// A concept entry within a group: its link text, filename stem, and description.
struct Entry {
    text: String,
    file: String,
    desc: Option<String>,
}

/// Group a directory's direct concepts by `type`, sorted by heading then entry
/// text, so the listing is deterministic.
fn concept_groups(concepts: &[&ConceptSummary]) -> Vec<ConceptGroup> {
    let mut by_type: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    for concept in concepts {
        let stem = last_segment(concept.id.as_str()).to_owned();
        let text = concept.title.clone().unwrap_or_else(|| stem.clone());
        by_type
            .entry(
                concept
                    .type_
                    .clone()
                    .unwrap_or_else(|| "Concepts".to_owned()),
            )
            .or_default()
            .push(Entry {
                text,
                file: stem,
                desc: concept.description.as_deref().map(collapse_ws),
            });
    }
    by_type
        .into_iter()
        .map(|(heading, mut entries)| {
            entries.sort_by(|a, b| {
                a.text
                    .to_lowercase()
                    .cmp(&b.text.to_lowercase())
                    .then_with(|| a.text.cmp(&b.text))
            });
            ConceptGroup { heading, entries }
        })
        .collect()
}

/// Render one index file: a `# Subdirectories` block (when any) followed by a
/// block per concept type, blocks separated by a blank line, one trailing
/// newline, and any preserved root frontmatter prepended.
fn render(subdirs: &[SubdirEntry], groups: &[ConceptGroup], frontmatter: Option<&str>) -> String {
    let mut blocks: Vec<String> = Vec::new();

    if !subdirs.is_empty() {
        let mut block = String::from("# Subdirectories\n");
        for sub in subdirs {
            let _ = write!(block, "\n* [{}]({}/{INDEX_FILENAME})", sub.name, sub.name);
            if let Some(desc) = &sub.desc {
                let _ = write!(block, " - {desc}");
            }
        }
        blocks.push(block);
    }

    for group in groups {
        let mut block = format!("# {}\n", group.heading);
        for entry in &group.entries {
            let _ = write!(block, "\n* [{}]({}.md)", entry.text, entry.file);
            if let Some(desc) = &entry.desc {
                let _ = write!(block, " - {desc}");
            }
        }
        blocks.push(block);
    }

    let body = blocks.join("\n\n");
    match frontmatter {
        Some(fm) => format!("{fm}\n\n{body}\n"),
        None => format!("{body}\n"),
    }
}

/// Map each `[text](target)` list entry in an index body to its trailing
/// ` - description`, used to carry human-written subdirectory blurbs across a
/// regeneration.
fn parse_entry_descriptions(content: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if !(trimmed.starts_with("* ") || trimmed.starts_with("- ")) {
            continue;
        }
        let Some((target, rest)) = link_target(trimmed) else {
            continue;
        };
        if let Some(desc) = rest.strip_prefix(" - ") {
            let desc = desc.trim();
            if !desc.is_empty() {
                map.insert(target.to_owned(), desc.to_owned());
            }
        }
    }
    map
}

/// Split a `[text](target)…` bullet into its link target and whatever follows the
/// closing paren. `None` if the line is not an inline markdown link.
fn link_target(line: &str) -> Option<(&str, &str)> {
    let open = line.find("](")? + 2;
    let close = line.get(open..)?.find(')')?;
    let target = line.get(open..open + close)?;
    let rest = line.get(open + close + 1..)?;
    Some((target, rest))
}

/// Extract a leading `---`-delimited frontmatter block (without its trailing
/// newline), or `None` if the content does not open with one.
fn leading_frontmatter(content: &str) -> Option<&str> {
    let (_, body) = frontmatter::split_document(content)?;
    let block_end = content.len() - body.len();
    content
        .get(..block_end)
        .map(|block| block.trim_end_matches(['\n', '\r']))
}

/// The bundle-relative index path for a directory (`""` → `index.md`).
fn index_path(dir: &str) -> String {
    if dir.is_empty() {
        INDEX_FILENAME.to_owned()
    } else {
        format!("{dir}/{INDEX_FILENAME}")
    }
}

/// Split a concept id into its directory (`""` at the root) and filename stem.
fn split_id(id: &str) -> (String, &str) {
    match id.rsplit_once('/') {
        Some((dir, stem)) => (dir.to_owned(), stem),
        None => (String::new(), id),
    }
}

/// The parent directory of a directory path (`""` at the root).
fn parent_dir(dir: &str) -> String {
    dir.rsplit_once('/')
        .map_or(String::new(), |(p, _)| p.to_owned())
}

/// The last `/`-separated segment of a path.
fn last_segment(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, seg)| seg)
}

/// Collapse all runs of whitespace (including newlines from folded YAML) to a
/// single space so a multi-line description renders as one bullet line.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn id(s: &str) -> ConceptId {
        ConceptId::from_relative_path(std::path::Path::new(&format!("{s}.md"))).unwrap()
    }

    fn summary(
        id_: &str,
        type_: Option<&str>,
        title: Option<&str>,
        desc: Option<&str>,
    ) -> ConceptSummary {
        ConceptSummary {
            id: id(id_),
            type_: type_.map(str::to_owned),
            title: title.map(str::to_owned),
            description: desc.map(str::to_owned),
        }
    }

    fn plan_no_existing(concepts: &[ConceptSummary]) -> Vec<PlannedIndex> {
        plan(concepts, &|_| None)
    }

    #[test]
    fn groups_concepts_by_type_under_their_directory() {
        let plan = plan_no_existing(&[
            summary(
                "tables/orders",
                Some("Table"),
                Some("Orders"),
                Some("All orders."),
            ),
            summary("tables/customers", Some("Table"), Some("Customers"), None),
        ]);
        let tables = plan.iter().find(|p| p.path == "tables/index.md").unwrap();
        assert_eq!(
            tables.content,
            "# Table\n\n* [Customers](customers.md)\n* [Orders](orders.md) - All orders.\n"
        );
    }

    #[test]
    fn root_lists_subdirectories() {
        let plan = plan_no_existing(&[summary(
            "tables/orders",
            Some("Table"),
            Some("Orders"),
            None,
        )]);
        let root = plan.iter().find(|p| p.path == "index.md").unwrap();
        assert_eq!(
            root.content,
            "# Subdirectories\n\n* [tables](tables/index.md)\n"
        );
    }

    #[test]
    fn preserves_subdirectory_descriptions_from_the_prior_index() {
        let concepts = [summary(
            "tables/orders",
            Some("Table"),
            Some("Orders"),
            None,
        )];
        let existing = |path: &str| {
            (path == "index.md").then(|| {
                "# Subdirectories\n\n* [tables](tables/index.md) - The warehouse tables.\n"
                    .to_owned()
            })
        };
        let plan = plan(&concepts, &existing);
        let root = plan.iter().find(|p| p.path == "index.md").unwrap();
        assert_eq!(
            root.content,
            "# Subdirectories\n\n* [tables](tables/index.md) - The warehouse tables.\n"
        );
    }

    #[test]
    fn preserves_root_frontmatter() {
        let concepts = [summary(
            "tables/orders",
            Some("Table"),
            Some("Orders"),
            None,
        )];
        let existing = |path: &str| {
            (path == "index.md")
                .then(|| "---\nokf_version: \"0.1\"\n---\n\n# Subdirectories\n".to_owned())
        };
        let plan = plan(&concepts, &existing);
        let root = plan.iter().find(|p| p.path == "index.md").unwrap();
        assert_eq!(
            root.content,
            "---\nokf_version: \"0.1\"\n---\n\n# Subdirectories\n\n* [tables](tables/index.md)\n"
        );
    }

    #[test]
    fn intermediate_directory_with_only_subdirs_is_planned() {
        let plan = plan_no_existing(&[summary(
            "references/metrics/wau",
            Some("Metric"),
            Some("WAU"),
            None,
        )]);
        let refs = plan
            .iter()
            .find(|p| p.path == "references/index.md")
            .unwrap();
        assert_eq!(
            refs.content,
            "# Subdirectories\n\n* [metrics](metrics/index.md)\n"
        );
        // Every ancestor directory is planned: root, references, references/metrics.
        let paths: Vec<&str> = plan.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "index.md",
                "references/index.md",
                "references/metrics/index.md"
            ]
        );
    }

    #[test]
    fn untyped_concept_groups_under_concepts_heading() {
        let plan = plan_no_existing(&[summary("notes/x", None, None, None)]);
        let notes = plan.iter().find(|p| p.path == "notes/index.md").unwrap();
        assert_eq!(notes.content, "# Concepts\n\n* [x](x.md)\n");
    }

    #[test]
    fn folded_description_collapses_to_one_line() {
        let plan = plan_no_existing(&[summary(
            "t/a",
            Some("T"),
            Some("A"),
            Some("one\ntwo  three"),
        )]);
        let dir = plan.iter().find(|p| p.path == "t/index.md").unwrap();
        assert!(dir.content.contains("* [A](a.md) - one two three\n"));
    }

    #[test]
    fn empty_bundle_plans_nothing() {
        assert!(plan_no_existing(&[]).is_empty());
    }
}
