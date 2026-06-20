//! Loading a directory tree into an in-memory bundle model.
//!
//! Loading is **permissive** (SPEC §9): a single unreadable or malformed file
//! never aborts the load. Such files are recorded in [`Bundle::parse_errors`]
//! and skipped, and broken cross-links are retained as graph edges to
//! non-existent concepts. Both are legal per the spec.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::core::concept::{Concept, ConceptParseError};
use crate::core::concept_id::ConceptId;
use crate::error::{Error, Result};

/// The two filenames reserved at every level of the hierarchy (SPEC §3.1).
/// They are never treated as concepts.
const RESERVED_FILENAMES: [&str; 2] = ["index.md", "log.md"];

/// An in-memory OKF bundle: its concepts plus the cross-link graph.
#[derive(Debug)]
pub struct Bundle {
    root: PathBuf,
    /// Concepts keyed by ID, ordered by ID for deterministic iteration.
    concepts: BTreeMap<ConceptId, Concept>,
    /// Reverse link graph: for each concept, the IDs that link to it.
    backlinks: BTreeMap<ConceptId, Vec<ConceptId>>,
    /// Files that could not be loaded, in walk order.
    parse_errors: Vec<ParseError>,
}

impl Bundle {
    /// Load a bundle rooted at `root`, walking the tree for `.md` files.
    ///
    /// # Errors
    ///
    /// Returns [`Error::BundleNotADirectory`] if `root` is not an existing
    /// directory. Per-file failures do not error; they are collected in
    /// [`Bundle::parse_errors`].
    pub fn load(root: &Path) -> Result<Self> {
        if !root.is_dir() {
            return Err(Error::BundleNotADirectory {
                path: root.to_path_buf(),
            });
        }

        let mut concepts = BTreeMap::new();
        let mut parse_errors = Vec::new();

        // Collect entries first and sort by path so load order — and therefore
        // the order of `parse_errors` — is deterministic across platforms.
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in WalkDir::new(root).sort_by_file_name() {
            match entry {
                Ok(entry) if entry.file_type().is_file() => {
                    if entry.path().extension().is_some_and(|e| e == "md") {
                        files.push(entry.into_path());
                    }
                }
                Ok(_) => {}
                Err(err) => parse_errors.push(ParseError {
                    path: err.path().map(Path::to_path_buf).unwrap_or_default(),
                    kind: ParseErrorKind::Walk(err.to_string()),
                }),
            }
        }

        // Read and parse files in parallel — the dominant cost over a large
        // bundle is the per-file I/O and markdown parse, and each file is
        // independent. `par_iter` preserves input order on `collect`, so the
        // sequential merge below stays deterministic regardless of thread
        // scheduling.
        let outcomes: Vec<(PathBuf, FileOutcome)> = files
            .par_iter()
            .filter_map(|path| {
                let relative = path.strip_prefix(root).ok()?;
                if is_reserved(relative) {
                    return None;
                }
                let id = ConceptId::from_relative_path(relative)?;
                Some((relative.to_path_buf(), load_file(path, id)))
            })
            .collect();

        for (relative, outcome) in outcomes {
            match outcome {
                FileOutcome::Loaded(concept) => {
                    concepts.insert(concept.id.clone(), *concept);
                }
                FileOutcome::Read(msg) => parse_errors.push(ParseError {
                    path: relative,
                    kind: ParseErrorKind::Read(msg),
                }),
                FileOutcome::Parse(err) => parse_errors.push(ParseError {
                    path: relative,
                    kind: ParseErrorKind::Parse(err),
                }),
            }
        }

        let backlinks = build_backlinks(&concepts);

        Ok(Self {
            root: root.to_path_buf(),
            concepts,
            backlinks,
            parse_errors,
        })
    }

    /// The bundle's root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// All successfully loaded concepts, ordered by ID.
    #[must_use = "iterator is lazy and does nothing unless consumed"]
    pub fn concepts(&self) -> impl ExactSizeIterator<Item = &Concept> {
        self.concepts.values()
    }

    /// The number of loaded concepts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.concepts.len()
    }

    /// Whether the bundle contains no loaded concepts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.concepts.is_empty()
    }

    /// Look up a concept by ID.
    #[must_use]
    pub fn get(&self, id: &ConceptId) -> Option<&Concept> {
        self.concepts.get(id)
    }

    /// Whether a concept with this ID was loaded (a link target "exists").
    #[must_use]
    pub fn contains(&self, id: &ConceptId) -> bool {
        self.concepts.contains_key(id)
    }

    /// The IDs that link to `id` ("cited by"), ordered.
    #[must_use]
    pub fn backlinks(&self, id: &ConceptId) -> &[ConceptId] {
        self.backlinks.get(id).map_or(&[], Vec::as_slice)
    }

    /// The distinct concept IDs that `id` links to ("outbound"), sorted.
    ///
    /// Mirrors [`Bundle::backlinks`] for the forward direction. A document may
    /// link to the same target more than once and in any order; the result is
    /// deduplicated and sorted so the graph contract is stable. Targets that
    /// resolve but are absent from the bundle (broken links) are included, just
    /// as they are in the reverse graph. Returns an empty slice's worth of work
    /// (an empty `Vec`) when `id` is not a loaded concept — its outbound links
    /// are unknowable without its body.
    #[must_use]
    pub fn outbound(&self, id: &ConceptId) -> Vec<ConceptId> {
        let Some(concept) = self.concepts.get(id) else {
            return Vec::new();
        };
        let mut targets: Vec<ConceptId> = concept
            .links
            .iter()
            .filter_map(|link| link.target.clone())
            .collect();
        targets.sort_unstable();
        targets.dedup();
        targets
    }

    /// Files that could not be loaded, in walk order.
    #[must_use]
    pub fn parse_errors(&self) -> &[ParseError] {
        &self.parse_errors
    }
}

/// Breadth-first walk from `start` following `adjacency`, recording the shortest
/// distance to each distinct concept up to `depth` hops.
///
/// The start node is excluded from the result, which is sorted nearest-first then
/// by ID for a deterministic order. Each node is enqueued at most once, so the
/// cost is linear in the edges of the visited neighborhood, independent of the
/// bundle's overall size.
#[must_use]
pub fn bfs<F>(start: &ConceptId, depth: usize, adjacency: F) -> Vec<(ConceptId, usize)>
where
    F: Fn(&ConceptId) -> Vec<ConceptId>,
{
    let mut seen: std::collections::HashSet<ConceptId> =
        std::collections::HashSet::from([start.clone()]);
    let mut frontier = vec![start.clone()];
    let mut found: Vec<(ConceptId, usize)> = Vec::new();

    for distance in 1..=depth {
        let mut next = Vec::new();
        for node in &frontier {
            for neighbor in adjacency(node) {
                if seen.insert(neighbor.clone()) {
                    found.push((neighbor.clone(), distance));
                    next.push(neighbor);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    found.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    found
}

/// The result of reading and parsing a single candidate file.
///
/// Carries no path; the caller pairs it back with the file's bundle-relative
/// path so error reporting and the concept map stay deterministic.
enum FileOutcome {
    /// A successfully parsed concept (boxed to keep the enum small).
    Loaded(Box<Concept>),
    /// The file could not be read (I/O error, non-UTF-8 contents).
    Read(String),
    /// The file was read but could not be parsed as a concept.
    Parse(ConceptParseError),
}

/// Read, parse, and timestamp a single concept file.
fn load_file(path: &Path, id: ConceptId) -> FileOutcome {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => return FileOutcome::Read(err.to_string()),
    };
    match Concept::parse(id, &content) {
        Ok(mut concept) => {
            concept.modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            FileOutcome::Loaded(Box::new(concept))
        }
        Err(err) => FileOutcome::Parse(err),
    }
}

/// Whether a bundle-relative path is one of the reserved filenames at any level.
fn is_reserved(relative: &Path) -> bool {
    relative
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| RESERVED_FILENAMES.contains(&name))
}

/// Build the reverse link graph from resolved outbound links.
///
/// Includes edges to non-existent concepts (broken links) so that both halves
/// of the graph stay faithful to what producers wrote.
fn build_backlinks(concepts: &BTreeMap<ConceptId, Concept>) -> BTreeMap<ConceptId, Vec<ConceptId>> {
    let mut backlinks: BTreeMap<ConceptId, Vec<ConceptId>> = BTreeMap::new();
    for concept in concepts.values() {
        for link in &concept.links {
            if let Some(target) = &link.target {
                let citing = backlinks.entry(target.clone()).or_default();
                // A document may link to the same target more than once; only
                // record it as a backlink once.
                if citing.last() != Some(&concept.id) && !citing.contains(&concept.id) {
                    citing.push(concept.id.clone());
                }
            }
        }
    }
    backlinks
}

/// A file that could not be loaded into the bundle.
#[derive(Debug)]
pub struct ParseError {
    /// The bundle-relative path of the offending file.
    pub path: PathBuf,
    /// What went wrong.
    pub kind: ParseErrorKind,
}

/// The reason a file could not be loaded.
#[derive(Debug)]
pub enum ParseErrorKind {
    /// The directory walk failed to access an entry.
    Walk(String),
    /// The file could not be read (I/O error, non-UTF-8 contents).
    Read(String),
    /// The file was read but could not be parsed as a concept.
    Parse(ConceptParseError),
}

impl ParseError {
    /// The failure reason on its own, without the file path prefix the
    /// [`fmt::Display`] rendering adds.
    #[must_use]
    pub fn reason(&self) -> String {
        match &self.kind {
            ParseErrorKind::Walk(msg) | ParseErrorKind::Read(msg) => msg.clone(),
            ParseErrorKind::Parse(err) => err.to_string(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.reason())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn loads_concepts_and_skips_reserved_files() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, "index.md", "# Bundle\n");
        write(root, "log.md", "# 2026-06-20\n");
        write(root, "tables/orders.md", "---\ntype: Table\n---\nbody\n");
        write(root, "tables/index.md", "# Tables\n");

        let bundle = Bundle::load(root).unwrap();
        assert_eq!(bundle.len(), 1);
        let id = ConceptId::from_relative_path(Path::new("tables/orders.md")).unwrap();
        assert!(bundle.contains(&id));
    }

    #[test]
    fn collects_parse_errors_without_aborting() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(root, "good.md", "---\ntype: Metric\n---\nok\n");
        write(root, "bad.md", "no frontmatter here\n");

        let bundle = Bundle::load(root).unwrap();
        assert_eq!(bundle.len(), 1);
        assert_eq!(bundle.parse_errors().len(), 1);
        assert_eq!(
            bundle.parse_errors().first().map(|e| e.path.clone()),
            Some(PathBuf::from("bad.md"))
        );
    }

    #[test]
    fn builds_backlinks_including_broken_links() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(
            root,
            "a.md",
            "---\ntype: T\n---\nlinks [b](/b.md) and [ghost](/ghost.md)\n",
        );
        write(root, "b.md", "---\ntype: T\n---\nno links\n");

        let bundle = Bundle::load(root).unwrap();
        let a = ConceptId::from_relative_path(Path::new("a.md")).unwrap();
        let b = ConceptId::from_relative_path(Path::new("b.md")).unwrap();
        let ghost = ConceptId::from_relative_path(Path::new("ghost.md")).unwrap();

        assert_eq!(bundle.backlinks(&b), std::slice::from_ref(&a));
        // The broken link still produces a backlink edge to a non-existent ID.
        assert_eq!(bundle.backlinks(&ghost), std::slice::from_ref(&a));
        assert!(!bundle.contains(&ghost));
    }

    #[test]
    fn outbound_is_deduped_sorted_and_includes_broken_links() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write(
            root,
            "a.md",
            "---\ntype: T\n---\nlinks [c](/c.md), [b](/b.md), and [c again](/c.md), \
             plus [ghost](/ghost.md)\n",
        );
        write(root, "b.md", "---\ntype: T\n---\nno links\n");
        write(root, "c.md", "---\ntype: T\n---\nno links\n");

        let bundle = Bundle::load(root).unwrap();
        let a = ConceptId::from_relative_path(Path::new("a.md")).unwrap();
        assert_eq!(
            bundle.outbound(&a),
            vec![
                ConceptId::from_relative_path(Path::new("b.md")).unwrap(),
                ConceptId::from_relative_path(Path::new("c.md")).unwrap(),
                ConceptId::from_relative_path(Path::new("ghost.md")).unwrap(),
            ]
        );
        // A concept the bundle never loaded has no knowable outbound links.
        let ghost = ConceptId::from_relative_path(Path::new("ghost.md")).unwrap();
        assert!(bundle.outbound(&ghost).is_empty());
    }

    #[test]
    fn missing_root_is_an_error() {
        let err = Bundle::load(Path::new("/no/such/bundle/here")).unwrap_err();
        assert!(matches!(err, Error::BundleNotADirectory { .. }));
    }
}
