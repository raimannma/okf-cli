//! A single concept: identity, frontmatter, body, and outbound links.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use crate::core::concept_id::ConceptId;
use crate::core::frontmatter::{self, Frontmatter, FrontmatterError};

/// One unit of knowledge — a parsed OKF markdown document (SPEC §2).
#[derive(Debug, Clone)]
pub struct Concept {
    /// The concept's identity (bundle-relative path without `.md`).
    pub id: ConceptId,
    /// The parsed YAML frontmatter.
    pub frontmatter: Frontmatter,
    /// The markdown body, verbatim (everything after the frontmatter block).
    pub body: String,
    /// Outbound markdown links found in the body, in document order.
    pub links: Vec<Link>,
    /// The source file's last-modified time, when the loader could stat it.
    ///
    /// Used as the fallback "modified" signal when a concept carries no
    /// `timestamp` frontmatter field. `None` if the metadata was unavailable.
    pub modified: Option<std::time::SystemTime>,
}

/// A markdown link from this concept to another target.
#[derive(Debug, Clone)]
pub struct Link {
    /// The raw link target exactly as written in the document.
    pub raw: String,
    /// The concept it resolves to, or `None` for external/non-concept targets.
    ///
    /// A resolved target may still be absent from the bundle (a legal broken
    /// link); resolution does not check existence.
    pub target: Option<ConceptId>,
}

impl Concept {
    /// Parse a concept from its bundle-relative `id` and raw file `content`.
    ///
    /// # Errors
    ///
    /// Returns [`ConceptParseError::MissingFrontmatter`] if the document does
    /// not open with a `---` frontmatter block, or
    /// [`ConceptParseError::Frontmatter`] if that block is not valid.
    pub fn parse(id: ConceptId, content: &str) -> Result<Self, ConceptParseError> {
        let (yaml, body) =
            frontmatter::split_document(content).ok_or(ConceptParseError::MissingFrontmatter)?;
        let frontmatter = Frontmatter::parse(yaml).map_err(ConceptParseError::Frontmatter)?;
        let links = extract_links(&id, body);
        Ok(Self {
            id,
            frontmatter,
            body: body.to_owned(),
            links,
            modified: None,
        })
    }

    /// Whether any outbound link in the body resolves to `target`.
    ///
    /// Counts a target reached by any of its links, in any section or inline in
    /// prose — so an idempotent `link` can tell a concept already cites another
    /// without caring how the existing link was written.
    #[must_use]
    pub fn links_to(&self, target: &ConceptId) -> bool {
        self.links.iter().any(|l| l.target.as_ref() == Some(target))
    }

    /// The content under `heading` with its heading line removed and surrounding
    /// blank lines trimmed, matched with the same rules as [`section`](Self::section).
    /// `None` if no section matches; `Some("")` for a section with no content.
    #[must_use]
    pub fn section_content(&self, heading: &str) -> Option<&str> {
        let section = self.section(heading)?;
        let rest = section.split_once('\n').map_or("", |(_, rest)| rest);
        Some(rest.trim_matches('\n'))
    }

    /// Produce a new body with every list-item line whose markdown link resolves
    /// to `target` removed, returning the new body and how many lines were dropped.
    ///
    /// Only list items are touched — the bullet form that [`okf link`] writes and
    /// that bundles use for "related" lists (SPEC §5 examples) — so a link buried
    /// in prose is left intact rather than mangling a sentence. The rest of the
    /// body is spliced through untouched.
    #[must_use]
    pub fn without_link(&self, target: &ConceptId) -> (String, usize) {
        self.without_links(std::slice::from_ref(target))
    }

    /// As [`without_link`](Self::without_link), but drops list items linking to
    /// *any* of `targets` — the primitive `rm` uses to scrub every bullet pointing
    /// at a deleted concept in one pass. An empty `targets` removes nothing.
    #[must_use]
    pub fn without_links(&self, targets: &[ConceptId]) -> (String, usize) {
        let mut removed = 0usize;
        let mut kept = String::with_capacity(self.body.len());
        for line in self.body.split_inclusive('\n') {
            if is_list_item(line) && targets.iter().any(|t| line_links_to(&self.id, line, t)) {
                removed += 1;
                continue;
            }
            kept.push_str(line);
        }
        if removed == 0 {
            return (self.body.clone(), 0);
        }
        (kept, removed)
    }

    /// Extract a single section of the body, identified by its heading.
    ///
    /// `heading` may be written with or without leading `#`s — both `"# Schema"`
    /// and `"Schema"` match a `# Schema` heading. When `#`s are given the heading
    /// level must also match; given none, any level matches. The returned slice
    /// runs from the matching heading line up to (but excluding) the next heading
    /// of the same or higher level, or the end of the body. Matching is on the
    /// trimmed heading text. Returns `None` if no heading matches.
    #[must_use]
    pub fn section(&self, heading: &str) -> Option<&str> {
        let (start, end) = self.section_span(heading)?;
        self.body.get(start..end)
    }

    /// The byte range `[start, end)` of the `heading` section within
    /// [`body`](Self::body), matched with the same rules as [`section`](Self::section).
    /// `start` is the heading's first byte; `end` is the start of the next heading
    /// of the same or higher level (or the body's end). `None` if nothing matches.
    fn section_span(&self, heading: &str) -> Option<(usize, usize)> {
        let (want_level, want_title) = parse_heading_query(heading);
        let headings = collect_headings(&self.body);

        let (idx, matched) = headings
            .iter()
            .enumerate()
            .find(|(_, h)| h.title == want_title && (want_level == 0 || h.level == want_level))?;

        let end = headings
            .iter()
            .skip(idx + 1)
            .find(|h| h.level <= matched.level)
            .map_or(self.body.len(), |h| h.start);

        Some((matched.start, end))
    }

    /// Produce a new body with the `heading` section's content replaced by (or, when
    /// `append`, extended with) `content`, returning the new body and which kind of
    /// edit happened.
    ///
    /// The heading line is owned by the document, not the caller: `content` is the
    /// section *body*, and the existing `#` heading is preserved verbatim. When no
    /// section matches, a new one is created at the end of the body, its heading
    /// rendered from `heading` (a missing `#` level defaults to level 1). Sections
    /// stay separated by a blank line and the body keeps a single trailing newline,
    /// so re-running an identical edit is a no-op. Only the targeted section's bytes
    /// move; the rest of the body is spliced through untouched.
    #[must_use]
    pub fn with_section(
        &self,
        heading: &str,
        content: &str,
        append: bool,
    ) -> (String, SectionEdit) {
        let content = content.trim_matches('\n');
        match self.section_span(heading) {
            Some((start, end)) => self.edit_section(start, end, content, append),
            None => (self.append_section(heading, content), SectionEdit::Created),
        }
    }

    /// Append a brand-new section to the end of the body.
    fn append_section(&self, heading: &str, content: &str) -> String {
        let block = render_section(&canonical_heading(heading), content, "\n");
        let prefix = self.body.trim_end_matches('\n');
        if prefix.is_empty() {
            block
        } else {
            format!("{prefix}\n\n{block}")
        }
    }

    /// Replace or append within the existing section spanning `start..end`,
    /// keeping its heading line and splicing the rest of the body through.
    fn edit_section(
        &self,
        start: usize,
        end: usize,
        content: &str,
        append: bool,
    ) -> (String, SectionEdit) {
        let section = self.body.get(start..end).unwrap_or_default();
        let heading_len = section.find('\n').unwrap_or(section.len());
        let heading_line = section.get(..heading_len).unwrap_or_default().trim_end();

        let (new_content, edit) = if append {
            let existing = section
                .get(heading_len..)
                .unwrap_or_default()
                .trim_matches('\n');
            let combined = match (existing.is_empty(), content.is_empty()) {
                (true, _) => content.to_owned(),
                (false, true) => existing.to_owned(),
                (false, false) => format!("{existing}\n\n{content}"),
            };
            (combined, SectionEdit::Appended)
        } else {
            (content.to_owned(), SectionEdit::Replaced)
        };

        // A non-final section keeps a blank line before the next heading; the last
        // section ends in a single newline.
        let trailing = if end < self.body.len() { "\n\n" } else { "\n" };
        let block = render_section(heading_line, &new_content, trailing);

        let mut body = String::with_capacity(self.body.len() + block.len());
        body.push_str(self.body.get(..start).unwrap_or_default());
        body.push_str(&block);
        body.push_str(self.body.get(end..).unwrap_or_default());
        (body, edit)
    }

    /// Rewrite the destinations of outbound markdown links in the body.
    ///
    /// For each inline link/image, `retarget` is called with the link's raw
    /// destination and the concept it resolves to against this concept's id
    /// (`None` for external/non-concept targets). Returning `Some(new_dest)`
    /// replaces just that destination text in place; `None` leaves the link
    /// alone. Returns the new body and how many destinations were rewritten.
    ///
    /// Only inline `[text](dest)` destinations are touched — reference-style and
    /// autolink forms have no inline destination to edit and are left intact (OKF
    /// bundles author inline links, SPEC §5). The rest of the body, including the
    /// link text, is spliced through byte-for-byte.
    #[must_use]
    pub fn rewrite_link_dests(
        &self,
        retarget: impl Fn(&str, Option<&ConceptId>) -> Option<String>,
    ) -> (String, usize) {
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        for (event, range) in Parser::new(&self.body).into_offset_iter() {
            let Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) = event
            else {
                continue;
            };
            let dest = dest_url.into_string();
            if dest.is_empty() {
                continue;
            }
            let resolved = self.id.resolve_link(&dest);
            let Some(new_dest) = retarget(&dest, resolved.as_ref()) else {
                continue;
            };
            if new_dest == dest {
                continue;
            }
            // Locate the destination inside the link span: it begins just after
            // the `](` that closes the link text. A span without `](` is a
            // reference-style link or autolink, which has no inline dest to edit.
            let span = self.body.get(range.clone()).unwrap_or_default();
            let Some(bracket) = span.find("](") else {
                continue;
            };
            let search_from = bracket + 2;
            let Some(rel) = span.get(search_from..).and_then(|s| s.find(&dest)) else {
                continue;
            };
            let start = range.start + search_from + rel;
            edits.push((start, start + dest.len(), new_dest));
        }

        if edits.is_empty() {
            return (self.body.clone(), 0);
        }

        edits.sort_by_key(|(start, _, _)| *start);
        let mut out = String::with_capacity(self.body.len());
        let mut cursor = 0usize;
        let mut count = 0usize;
        for (start, end, replacement) in edits {
            // A nested link (image inside link text) can yield an edit inside an
            // outer one; skip it rather than splice an overlap.
            if start < cursor {
                continue;
            }
            out.push_str(self.body.get(cursor..start).unwrap_or_default());
            out.push_str(&replacement);
            cursor = end;
            count += 1;
        }
        out.push_str(self.body.get(cursor..).unwrap_or_default());
        (out, count)
    }
}

/// What [`Concept::with_section`] did to the targeted section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionEdit {
    /// The section existed and its content was overwritten.
    Replaced,
    /// The section existed and the content was added to its end.
    Appended,
    /// No section matched, so one was created at the end of the body.
    Created,
}

/// Render a section block: the heading line, a blank line, the content, then
/// `trailing`. An empty `content` collapses to just the heading plus `trailing`.
fn render_section(heading: &str, content: &str, trailing: &str) -> String {
    if content.is_empty() {
        format!("{heading}{trailing}")
    } else {
        format!("{heading}\n\n{content}{trailing}")
    }
}

/// Build a concrete heading line from a `--section` query for a newly created
/// section: a missing `#` level becomes level 1.
fn canonical_heading(query: &str) -> String {
    let (level, title) = parse_heading_query(query);
    format!("{} {title}", "#".repeat(level.max(1)))
}

/// A heading found in a body: its level, byte offset, and trimmed text.
struct HeadingSpan {
    level: usize,
    start: usize,
    title: String,
}

/// Split a `--section` query into its requested level (count of leading `#`s,
/// `0` meaning "any level") and its trimmed title text.
fn parse_heading_query(query: &str) -> (usize, String) {
    let trimmed = query.trim();
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    let title = trimmed.trim_start_matches('#').trim().to_owned();
    (level, title)
}

/// Collect every ATX/Setext heading in `body`, in document order.
fn collect_headings(body: &str) -> Vec<HeadingSpan> {
    let mut headings = Vec::new();
    let mut current: Option<(usize, usize, String)> = None;
    for (event, range) in Parser::new(body).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((level as usize, range.start, String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, _, title)) = current.as_mut() {
                    title.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, start, title)) = current.take() {
                    headings.push(HeadingSpan {
                        level,
                        start,
                        title: title.trim().to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
    headings
}

/// Extract outbound links from a markdown body, resolving each against `from`.
fn extract_links(from: &ConceptId, body: &str) -> Vec<Link> {
    let mut links = Vec::new();
    for event in Parser::new(body) {
        let Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) = event else {
            continue;
        };
        let raw = dest_url.into_string();
        let target = from.resolve_link(&raw);
        links.push(Link { raw, target });
    }
    links
}

/// Whether `line` begins (after indentation) with a markdown list marker — an
/// unordered bullet (`-`, `*`, `+`) or an ordered item (`1.`, `2)`), each
/// followed by a space.
fn is_list_item(line: &str) -> bool {
    let trimmed = line.trim_start();
    if matches!(trimmed.as_bytes(), [b'-' | b'*' | b'+', b' ', ..]) {
        return true;
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && matches!(
            trimmed.get(digits..).unwrap_or("").as_bytes(),
            [b'.' | b')', b' ', ..]
        )
}

/// Whether any markdown link on `line`, resolved as if written in `from`'s
/// document, points at `target`.
fn line_links_to(from: &ConceptId, line: &str, target: &ConceptId) -> bool {
    Parser::new(line).any(|event| {
        matches!(
            event,
            Event::Start(Tag::Link { ref dest_url, .. } | Tag::Image { ref dest_url, .. })
                if from.resolve_link(dest_url).as_ref() == Some(target)
        )
    })
}

/// A failure while parsing a single concept document.
#[derive(Debug, thiserror::Error)]
pub enum ConceptParseError {
    /// The document does not begin with a `---` frontmatter block.
    #[error("missing YAML frontmatter block (document must start with `---`)")]
    MissingFrontmatter,

    /// The frontmatter block could not be parsed.
    #[error(transparent)]
    Frontmatter(#[from] FrontmatterError),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn id(s: &str) -> ConceptId {
        ConceptId::from_relative_path(std::path::Path::new(&format!("{s}.md"))).unwrap()
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let content = "---\ntype: Metric\ntitle: WAU\n---\n\n# Definition\n\nbody text\n";
        let c = Concept::parse(id("metrics/wau"), content).unwrap();
        assert_eq!(c.frontmatter.type_(), Some("Metric"));
        assert_eq!(c.body, "\n# Definition\n\nbody text\n");
    }

    #[test]
    fn missing_frontmatter_is_an_error() {
        let err = Concept::parse(id("x"), "# Just a heading\n").unwrap_err();
        assert!(matches!(err, ConceptParseError::MissingFrontmatter));
    }

    #[test]
    fn unterminated_frontmatter_is_missing() {
        let err = Concept::parse(id("x"), "---\ntype: Metric\nno closing delimiter\n").unwrap_err();
        assert!(matches!(err, ConceptParseError::MissingFrontmatter));
    }

    #[test]
    fn extracts_and_resolves_links() {
        let content = "---\ntype: Table\n---\nSee [customers](/tables/customers.md) and \
                       [external](https://example.com).\n";
        let c = Concept::parse(id("tables/orders"), content).unwrap();
        assert_eq!(c.links.len(), 2);
        assert_eq!(
            c.links
                .first()
                .and_then(|l| l.target.as_ref())
                .map(ConceptId::as_str),
            Some("tables/customers")
        );
        assert!(c.links.get(1).is_some_and(|l| l.target.is_none()));
    }

    #[test]
    fn empty_frontmatter_block_parses() {
        let c = Concept::parse(id("x"), "---\n---\nbody\n").unwrap();
        assert!(c.frontmatter.is_empty());
        assert_eq!(c.body, "body\n");
    }

    const WITH_SECTIONS: &str = "---\ntype: Table\n---\n\
        # Overview\n\nintro text\n\n\
        # Schema\n\n- id\n- name\n\n\
        ## Indexes\n\nprimary key\n\n\
        # Joins\n\njoins here\n";

    #[test]
    fn section_returns_heading_content_up_to_next_same_level() {
        let c = Concept::parse(id("t"), WITH_SECTIONS).unwrap();
        let schema = c.section("# Schema").unwrap();
        // Includes the heading and its nested `## Indexes` subsection, stops at
        // the next `# Joins`.
        assert_eq!(
            schema,
            "# Schema\n\n- id\n- name\n\n## Indexes\n\nprimary key\n\n"
        );
    }

    #[test]
    fn section_matches_without_hashes() {
        let c = Concept::parse(id("t"), WITH_SECTIONS).unwrap();
        assert_eq!(c.section("Joins").unwrap(), "# Joins\n\njoins here\n");
    }

    #[test]
    fn section_requires_matching_level_when_hashes_given() {
        let c = Concept::parse(id("t"), WITH_SECTIONS).unwrap();
        // `Indexes` is level 2; asking for it at level 1 does not match.
        assert!(c.section("# Indexes").is_none());
        assert!(c.section("## Indexes").is_some());
    }

    #[test]
    fn section_runs_to_end_of_body_when_last() {
        let c = Concept::parse(id("t"), WITH_SECTIONS).unwrap();
        let indexes = c.section("## Indexes").unwrap();
        assert_eq!(indexes, "## Indexes\n\nprimary key\n\n");
    }

    #[test]
    fn unknown_section_is_none() {
        let c = Concept::parse(id("t"), WITH_SECTIONS).unwrap();
        assert!(c.section("# Nonexistent").is_none());
    }

    #[test]
    fn with_section_replaces_content_and_keeps_heading_and_neighbors() {
        let c = Concept::parse(id("t"), WITH_SECTIONS).unwrap();
        let (body, edit) = c.with_section("# Schema", "- only_col", false);
        assert_eq!(edit, SectionEdit::Replaced);
        // The `# Schema` heading and its trailing `## Indexes` subsection are
        // replaced; `# Overview` before and `# Joins` after are untouched.
        assert_eq!(
            body,
            "# Overview\n\nintro text\n\n# Schema\n\n- only_col\n\n# Joins\n\njoins here\n"
        );
    }

    #[test]
    fn with_section_appends_to_existing_content() {
        let c = Concept::parse(id("t"), WITH_SECTIONS).unwrap();
        let (body, edit) = c.with_section("# Joins", "- orders.id = items.order_id", true);
        assert_eq!(edit, SectionEdit::Appended);
        assert!(body.ends_with("# Joins\n\njoins here\n\n- orders.id = items.order_id\n"));
    }

    #[test]
    fn with_section_creates_missing_section_at_end() {
        let c = Concept::parse(id("t"), WITH_SECTIONS).unwrap();
        let (body, edit) = c.with_section("# Notes", "see ticket", false);
        assert_eq!(edit, SectionEdit::Created);
        assert!(body.ends_with("# Joins\n\njoins here\n\n# Notes\n\nsee ticket\n"));
    }

    #[test]
    fn with_section_creates_section_in_empty_body() {
        let c = Concept::parse(id("t"), "---\ntype: Table\n---\n").unwrap();
        let (body, edit) = c.with_section("Joins", "j", false);
        // No `#` given: the created heading defaults to level 1.
        assert_eq!(edit, SectionEdit::Created);
        assert_eq!(body, "# Joins\n\nj\n");
    }

    #[test]
    fn links_to_detects_an_outbound_link_by_resolved_target() {
        let content = "---\ntype: Table\n---\nSee [c](/tables/customers.md) and [x](../m/r.md).\n";
        let c = Concept::parse(id("tables/orders"), content).unwrap();
        assert!(c.links_to(&id("tables/customers")));
        assert!(c.links_to(&id("m/r")));
        assert!(!c.links_to(&id("tables/missing")));
    }

    #[test]
    fn section_content_strips_heading_and_surrounding_blanks() {
        let c = Concept::parse(id("t"), WITH_SECTIONS).unwrap();
        assert_eq!(c.section_content("# Joins").unwrap(), "joins here");
        assert_eq!(
            c.section_content("# Schema").unwrap(),
            "- id\n- name\n\n## Indexes\n\nprimary key"
        );
        assert!(c.section_content("# Nonexistent").is_none());
    }

    #[test]
    fn section_content_of_empty_section_is_empty() {
        let c = Concept::parse(id("t"), "---\ntype: T\n---\n# Related\n\n# Next\n\nx\n").unwrap();
        assert_eq!(c.section_content("# Related").unwrap(), "");
    }

    #[test]
    fn without_link_removes_matching_list_items_only() {
        let content = "---\ntype: Table\n---\n# Related\n\n\
            - [Customers](/tables/customers.md)\n\
            - [Items](/tables/items.md)\n\n\
            Prose mentioning [Customers](/tables/customers.md) inline.\n";
        let c = Concept::parse(id("tables/orders"), content).unwrap();
        let (body, removed) = c.without_link(&id("tables/customers"));
        // The bullet goes; the inline prose link stays.
        assert_eq!(removed, 1);
        assert_eq!(
            body,
            "# Related\n\n- [Items](/tables/items.md)\n\nProse mentioning \
             [Customers](/tables/customers.md) inline.\n"
        );
    }

    #[test]
    fn without_link_resolves_relative_targets_against_the_document() {
        let content = "---\ntype: Table\n---\n# Related\n\n- [C](./customers.md)\n";
        let c = Concept::parse(id("tables/orders"), content).unwrap();
        let (_, removed) = c.without_link(&id("tables/customers"));
        assert_eq!(removed, 1);
    }

    #[test]
    fn without_link_is_a_noop_when_nothing_matches() {
        let content = "---\ntype: Table\n---\n# Related\n\n- [Items](/tables/items.md)\n";
        let c = Concept::parse(id("tables/orders"), content).unwrap();
        let (body, removed) = c.without_link(&id("tables/customers"));
        assert_eq!(removed, 0);
        assert_eq!(body, c.body);
    }

    #[test]
    fn with_section_replace_is_idempotent() {
        let c = Concept::parse(id("t"), WITH_SECTIONS).unwrap();
        let (body, _) = c.with_section("# Joins", "joins here", false);
        let again = Concept::parse(id("t"), &format!("---\ntype: Table\n---\n{body}")).unwrap();
        let (body2, _) = again.with_section("# Joins", "joins here", false);
        assert_eq!(body, body2);
    }
}
