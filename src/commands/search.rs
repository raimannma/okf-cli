//! `search` subcommand: BM25-ranked keyword search over a bundle.
//!
//! A cheap retrieval step between the `list` survey and a full `get`: given a
//! query, return the concept IDs that best match, each with a short snippet, so
//! an agent can decide what to read without fetching bodies.
//!
//! Ranking is [Okapi BM25] over whole-word tokens (case-insensitive ASCII fold).
//! It is the field-boosted variant: each concept is one document whose ID,
//! frontmatter, and body contribute term frequencies weighted by [`WEIGHT_ID`],
//! [`WEIGHT_FRONTMATTER`], and [`WEIGHT_BODY`], with a single corpus-wide length
//! normalization. A concept is a hit if it contains **any** query term; BM25's
//! score then orders them, rewarding rarer terms (via IDF) and saturating
//! repeated ones.
//!
//! [Okapi BM25]: https://en.wikipedia.org/wiki/Okapi_BM25

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use rayon::prelude::*;

use crate::core::{Bundle, Concept};
use crate::error::{Error, Result};
use crate::output::OutputMode;

/// Per-field term-frequency boost: a token in the ID counts for more than one in
/// the frontmatter, which counts for more than one in the body.
const WEIGHT_ID: f32 = 3.0;
const WEIGHT_FRONTMATTER: f32 = 2.0;
const WEIGHT_BODY: f32 = 1.0;

/// BM25 term-frequency saturation (`k1`) and length-normalization (`b`) — the
/// standard defaults.
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Longest snippet returned, in characters (not bytes).
const SNIPPET_MAX_CHARS: usize = 160;

/// Search the bundle at `path` for `query`, printing the top `limit` hits whose
/// BM25 score is at least `min_score`.
///
/// `limit` of `0` means "no limit"; `min_score` of `0` keeps every match.
/// Ranking is deterministic: by descending BM25 score, then ascending concept ID
/// to break ties.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] if `query` has no searchable terms, or
/// [`Error::BundleNotADirectory`] if `path` is not a readable bundle.
pub(crate) fn run(
    path: &Path,
    query: &str,
    limit: usize,
    min_score: f64,
    mode: OutputMode,
) -> Result<()> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return Err(Error::InvalidInput(
            "search query has no searchable terms; provide at least one word".to_owned(),
        ));
    }

    let bundle = Bundle::load(path)?;
    let output = SearchOutput::from_bundle(&bundle, query, &terms, limit, min_score);
    let warnings = crate::output::bundle_warnings(&bundle);

    mode.emit(&output, &output.render_human(), &warnings);
    Ok(())
}

/// Tokenize a raw query into distinct ASCII-lowercased terms, in first-seen
/// order. Splitting and folding match the document tokenizer so query terms line
/// up with indexed terms.
fn query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for token in tokenize(query) {
        if !terms.contains(&token) {
            terms.push(token);
        }
    }
    terms
}

/// Split `text` into whole-word tokens: maximal runs of alphanumerics, each
/// ASCII-lowercased. Punctuation, whitespace, and `_` are separators.
fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
}

/// The stable JSON contract for `okf search`.
#[derive(Debug, serde::Serialize)]
struct SearchOutput<'a> {
    query: &'a str,
    /// Total concepts that matched and cleared the score floor, before `--limit`.
    total_matches: usize,
    results: Vec<SearchHit>,
    /// The `--min-score` floor in effect, and how many positive-scoring matches
    /// fell below it. Kept out of the JSON contract; only the human renderer uses
    /// them, to explain an empty result set.
    #[serde(skip)]
    min_score: f64,
    #[serde(skip)]
    filtered_below: usize,
}

/// One ranked hit: a concept and why it surfaced.
#[derive(Debug, serde::Serialize)]
struct SearchHit {
    id: String,
    #[serde(rename = "type")]
    type_: Option<String>,
    title: Option<String>,
    /// The BM25 relevance score, rounded to four decimals.
    score: f64,
    /// A short excerpt around the first match, for the agent to judge relevance.
    snippet: String,
}

impl<'a> SearchOutput<'a> {
    fn from_bundle(
        bundle: &Bundle,
        query: &'a str,
        terms: &[String],
        limit: usize,
        min_score: f64,
    ) -> Self {
        let concepts: Vec<&Concept> = bundle.concepts().collect();
        let index = Index::build(&concepts);

        let mut filtered_below = 0;
        let mut hits: Vec<SearchHit> = index
            .score(terms)
            .into_iter()
            .filter_map(|(doc, score)| {
                if score < min_score {
                    filtered_below += 1;
                    return None;
                }
                let concept = concepts.get(doc)?;
                Some(SearchHit {
                    id: concept.id.to_string(),
                    type_: concept.frontmatter.type_().map(str::to_owned),
                    title: concept.frontmatter.title().map(str::to_owned),
                    score: round4(score),
                    snippet: snippet(concept, terms),
                })
            })
            .collect();

        // Highest score first; ties broken by ID so output is stable.
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));

        let total_matches = hits.len();
        if limit != 0 {
            hits.truncate(limit);
        }

        Self {
            query,
            total_matches,
            results: hits,
            min_score,
            filtered_below,
        }
    }

    /// Render hits for human consumption, one per line, with a truncation note.
    fn render_human(&self) -> String {
        if self.results.is_empty() {
            if self.filtered_below > 0 {
                let (count, plural) = (
                    self.filtered_below,
                    if self.filtered_below == 1 { "" } else { "es" },
                );
                return format!(
                    "no matches at or above the --min-score floor of {}; \
                     {count} weaker match{plural} filtered out \
                     (lower --min-score to include {})\n",
                    self.min_score,
                    if self.filtered_below == 1 {
                        "it"
                    } else {
                        "them"
                    },
                );
            }
            return format!("no concepts matched {:?}\n", self.query);
        }

        let shown = self.results.len();
        let mut out = String::new();
        let _ = write!(
            out,
            "{} {} for {:?}",
            self.total_matches,
            if self.total_matches == 1 {
                "match"
            } else {
                "matches"
            },
            self.query,
        );
        if shown < self.total_matches {
            let _ = write!(out, " (showing top {shown})");
        }
        out.push_str("\n\n");

        let id_width = self
            .results
            .iter()
            .map(|h| h.id.chars().count())
            .max()
            .unwrap_or(0);
        let type_width = self
            .results
            .iter()
            .map(|h| h.type_.as_deref().unwrap_or("no type").chars().count() + 2)
            .max()
            .unwrap_or(0);

        for hit in &self.results {
            let bracketed = format!("[{}]", hit.type_.as_deref().unwrap_or("no type"));
            let _ = write!(
                out,
                "{:<id_width$}  {:<type_width$}  ({:.3})",
                hit.id, bracketed, hit.score
            );
            if !hit.snippet.is_empty() {
                out.push_str("  — ");
                out.push_str(&hit.snippet);
            }
            out.push('\n');
        }
        if shown < self.total_matches {
            let _ = writeln!(
                out,
                "… {} more (raise --limit to see them)",
                self.total_matches - shown
            );
        }
        out
    }
}

/// A transient in-memory BM25 index over the loaded concepts, rebuilt per query.
///
/// `postings[t]` lists every document containing token `t` with that document's
/// field-weighted term frequency; `doc_len[d]` is document `d`'s weighted length.
struct Index {
    postings: HashMap<String, Vec<(usize, f32)>>,
    doc_len: Vec<f64>,
    avg_len: f64,
    doc_count: f64,
}

impl Index {
    /// Build the index, tokenizing documents in parallel then folding their
    /// term frequencies into a shared postings map in deterministic doc order.
    fn build(concepts: &[&Concept]) -> Self {
        let per_doc: Vec<(Vec<(String, f32)>, f64)> =
            concepts.par_iter().map(|c| document_terms(c)).collect();

        let mut postings: HashMap<String, Vec<(usize, f32)>> = HashMap::new();
        let mut doc_len = Vec::with_capacity(per_doc.len());
        let mut total_len = 0.0;
        for (doc, (terms, len)) in per_doc.into_iter().enumerate() {
            doc_len.push(len);
            total_len += len;
            for (term, weight) in terms {
                postings.entry(term).or_default().push((doc, weight));
            }
        }

        let doc_count = count_to_f64(doc_len.len());
        let avg_len = if doc_count > 0.0 {
            total_len / doc_count
        } else {
            0.0
        };

        Self {
            postings,
            doc_len,
            avg_len,
            doc_count,
        }
    }

    /// Accumulate a BM25 score for every document containing any query term.
    ///
    /// Only the postings of the query terms are touched, so the cost scales with
    /// how many documents actually match, not with the corpus size.
    fn score(&self, terms: &[String]) -> Vec<(usize, f64)> {
        let mut scores = vec![0.0f64; self.doc_len.len()];
        for term in terms {
            let Some(postings) = self.postings.get(term) else {
                continue;
            };
            let idf = self.idf(count_to_f64(postings.len()));
            for &(doc, weight) in postings {
                let Some(len) = self.doc_len.get(doc) else {
                    continue;
                };
                let Some(slot) = scores.get_mut(doc) else {
                    continue;
                };
                *slot += idf * bm25_tf(f64::from(weight), *len, self.avg_len);
            }
        }
        scores
            .into_iter()
            .enumerate()
            .filter(|&(_, score)| score > 0.0)
            .collect()
    }

    /// Inverse document frequency for a term in `n_t` documents. The `1 + …`
    /// inside the log keeps it non-negative even for very common terms.
    fn idf(&self, n_t: f64) -> f64 {
        (1.0 + (self.doc_count - n_t + 0.5) / (n_t + 0.5)).ln()
    }
}

/// The BM25 term-frequency component for one term in one document.
fn bm25_tf(weighted_tf: f64, doc_len: f64, avg_len: f64) -> f64 {
    let norm = if avg_len > 0.0 {
        doc_len / avg_len
    } else {
        0.0
    };
    (weighted_tf * (K1 + 1.0)) / (weighted_tf + K1 * (1.0 - B + B * norm))
}

/// The field-weighted term frequencies of one concept and its weighted length.
fn document_terms(concept: &Concept) -> (Vec<(String, f32)>, f64) {
    let mut tf: HashMap<String, f32> = HashMap::new();
    let mut len = 0.0f64;
    let mut add = |text: &str, weight: f32| {
        for token in tokenize(text) {
            *tf.entry(token).or_insert(0.0) += weight;
            len += f64::from(weight);
        }
    };

    add(concept.id.as_str(), WEIGHT_ID);
    for value in concept.frontmatter.string_values() {
        add(value, WEIGHT_FRONTMATTER);
    }
    add(&concept.body, WEIGHT_BODY);

    (tf.into_iter().collect(), len)
}

/// Round to four decimal places, so scores are stable and readable in output.
fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// A document/corpus count as `f64`. Counts never approach 2^52 in any real
/// bundle, so the conversion is lossless in practice.
fn count_to_f64(n: usize) -> f64 {
    u32::try_from(n).map_or(f64::from(u32::MAX), f64::from)
}

/// Build a one-line snippet: the body line of the earliest matching term if any
/// term hits the body, otherwise the first matching frontmatter value.
fn snippet(concept: &Concept, terms: &[String]) -> String {
    let body_lower = concept.body.to_ascii_lowercase();
    if let Some(offset) = terms
        .iter()
        .filter_map(|t| find_token(&body_lower, t))
        .min()
    {
        return truncate_chars(line_around(&concept.body, offset).trim(), SNIPPET_MAX_CHARS);
    }
    for value in concept.frontmatter.string_values() {
        let lower = value.to_ascii_lowercase();
        if terms.iter().any(|t| find_token(&lower, t).is_some()) {
            return truncate_chars(value.trim(), SNIPPET_MAX_CHARS);
        }
    }
    String::new()
}

/// Byte offset of the first whole-word occurrence of `token` in an already
/// ASCII-lowercased `haystack`, or `None`. Boundaries are non-alphanumeric runs,
/// matching the tokenizer, so `user` does not match inside `username`.
fn find_token(haystack: &str, token: &str) -> Option<usize> {
    let mut start = 0;
    while let Some(found) = haystack.get(start..).and_then(|rest| rest.find(token)) {
        let at = start + found;
        let before = haystack.get(..at).and_then(|s| s.chars().next_back());
        let after = haystack
            .get(at + token.len()..)
            .and_then(|s| s.chars().next());
        let bounded = before.is_none_or(|c| !c.is_alphanumeric())
            && after.is_none_or(|c| !c.is_alphanumeric());
        if bounded {
            return Some(at);
        }
        start = at + token.len();
    }
    None
}

/// The line of `text` containing byte `offset` (newlines excluded).
fn line_around(text: &str, offset: usize) -> &str {
    let start = text
        .get(..offset)
        .and_then(|head| head.rfind('\n'))
        .map_or(0, |i| i + 1);
    let end = text
        .get(offset..)
        .and_then(|tail| tail.find('\n'))
        .map_or(text.len(), |i| offset + i);
    text.get(start..end).unwrap_or("")
}

/// Truncate to at most `max` characters, appending `…` when shortened.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
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
            "---\ntype: Table\ntitle: Customer Orders\ntags: [sales]\n---\n\
             # Schema\n\nThe orders table records each purchase.\n",
        )
        .unwrap();
        fs::write(
            root.join("tables/customers.md"),
            "---\ntype: Table\ntitle: Customers\n---\n\
             # Schema\n\nOne row per customer.\n",
        )
        .unwrap();
        fs::write(
            root.join("metrics.md"),
            "---\ntype: Metric\ntitle: Revenue\n---\n\
             Total revenue across all orders.\n",
        )
        .unwrap();
        dir
    }

    fn search(dir: &TempDir, query: &str, limit: usize) -> SearchOutput<'static> {
        search_min(dir, query, limit, 0.0)
    }

    fn search_min(
        dir: &TempDir,
        query: &str,
        limit: usize,
        min_score: f64,
    ) -> SearchOutput<'static> {
        let bundle = Bundle::load(dir.path()).unwrap();
        let terms = query_terms(query);
        let q: &'static str = Box::leak(query.to_owned().into_boxed_str());
        SearchOutput::from_bundle(&bundle, q, &terms, limit, min_score)
    }

    fn ids<'a>(output: &'a SearchOutput<'a>) -> Vec<&'a str> {
        output.results.iter().map(|h| h.id.as_str()).collect()
    }

    #[test]
    fn ranks_id_and_frontmatter_above_body() {
        let dir = fixture();
        let out = search(&dir, "orders", 0);
        // `tables/orders` carries "orders" in ID + title + body; `metrics` only
        // in its body, so it ranks lower. `customers` lacks the term entirely.
        assert_eq!(ids(&out), vec!["tables/orders", "metrics"]);
        let scores: Vec<f64> = out.results.iter().map(|h| h.score).collect();
        assert!(scores.windows(2).all(|w| w.first() > w.last()));
    }

    #[test]
    fn matches_any_term_and_rewards_more() {
        let dir = fixture();
        // OR semantics: "customer" hits orders' title and customers' body; "row"
        // only customers' body. Matching both, customers ranks first.
        let out = search(&dir, "customer row", 0);
        assert_eq!(
            out.results.first().map(|h| h.id.as_str()),
            Some("tables/customers")
        );
        assert!(ids(&out).contains(&"tables/orders"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let dir = fixture();
        let out = search(&dir, "REVENUE", 0);
        assert_eq!(ids(&out), vec!["metrics"]);
    }

    #[test]
    fn tokens_match_whole_words_not_substrings() {
        let dir = fixture();
        // "order" (singular) is not a token in any document; "orders" is.
        assert!(search(&dir, "order", 0).results.is_empty());
        assert!(!search(&dir, "orders", 0).results.is_empty());
    }

    #[test]
    fn no_match_is_empty() {
        let dir = fixture();
        let out = search(&dir, "nonexistentterm", 0);
        assert!(out.results.is_empty());
        assert_eq!(out.total_matches, 0);
    }

    #[test]
    fn no_match_renders_a_message() {
        let dir = fixture();
        let out = search(&dir, "nonexistentterm", 0);
        assert_eq!(
            out.render_human(),
            "no concepts matched \"nonexistentterm\"\n"
        );
    }

    #[test]
    fn all_filtered_message_points_at_min_score() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        for i in 0..60 {
            fs::write(
                root.join(format!("d{i:02}.md")),
                format!("---\ntype: T\n---\ncommon word number {i}\n"),
            )
            .unwrap();
        }
        // Every "common" match has near-zero IDF and is filtered by the floor.
        let out = search_min(&dir, "common", 0, 0.5);
        assert!(out.results.is_empty());
        let msg = out.render_human();
        assert!(msg.contains("--min-score floor of 0.5"), "{msg}");
        assert!(msg.contains("60 weaker matches filtered out"), "{msg}");
    }

    #[test]
    fn limit_truncates_but_reports_total() {
        let dir = fixture();
        let out = search(&dir, "schema", 1);
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.total_matches, 2);
    }

    #[test]
    fn snippet_comes_from_body_match() {
        let dir = fixture();
        let out = search(&dir, "purchase", 0);
        assert_eq!(
            out.results.first().map(|h| h.snippet.as_str()),
            Some("The orders table records each purchase.")
        );
    }

    #[test]
    fn snippet_falls_back_to_frontmatter() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("c.md"),
            "---\ntype: Metric\ntitle: Daily Active Users\n---\nunrelated body\n",
        )
        .unwrap();
        let out = search(&dir, "active", 0);
        assert_eq!(
            out.results.first().map(|h| h.snippet.as_str()),
            Some("Daily Active Users")
        );
    }

    #[test]
    fn rarer_terms_outrank_common_ones() {
        // A term in every document carries near-zero IDF; a rare one dominates.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        for i in 0..5 {
            fs::write(
                root.join(format!("d{i}.md")),
                format!("---\ntype: T\n---\ncommon word here number {i}\n"),
            )
            .unwrap();
        }
        fs::write(
            root.join("rare.md"),
            "---\ntype: T\n---\ncommon unicorn here\n",
        )
        .unwrap();

        let out = search(&dir, "common unicorn", 0);
        // The doc with the rare "unicorn" must top the ones sharing only "common".
        assert_eq!(out.results.first().map(|h| h.id.as_str()), Some("rare"));
    }

    #[test]
    fn min_score_drops_near_universal_term_matches() {
        // A term in (nearly) every document has IDF ~0, so its matches score
        // below the floor and are dropped; a rarer term still clears it.
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        for i in 0..60 {
            fs::write(
                root.join(format!("d{i:02}.md")),
                format!("---\ntype: T\n---\ncommon word number {i}\n"),
            )
            .unwrap();
        }
        fs::write(root.join("rare.md"), "---\ntype: T\n---\ncommon unicorn\n").unwrap();

        // Without a floor every "common" match is returned.
        assert_eq!(search_min(&dir, "common", 0, 0.0).total_matches, 61);
        // With the floor, the near-universal "common" matches vanish…
        assert!(search_min(&dir, "common", 0, 0.5).results.is_empty());
        // …but the rare "unicorn" still surfaces its one concept.
        assert_eq!(
            search_min(&dir, "unicorn", 0, 0.5)
                .results
                .first()
                .map(|h| h.id.as_str()),
            Some("rare")
        );
    }

    #[test]
    fn find_token_respects_word_boundaries() {
        assert_eq!(find_token("a username field", "user"), None);
        assert_eq!(find_token("the user field", "user"), Some(4));
        assert_eq!(find_token("user first", "user"), Some(0));
    }

    #[test]
    fn truncate_chars_appends_ellipsis() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
    }
}
