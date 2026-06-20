//! YAML frontmatter: an order-preserving key/value model with typed getters.
//!
//! Per SPEC §4.1 the only required field is `type`; everything else is optional
//! and producers may add arbitrary keys. Consumers MUST preserve unknown keys
//! and their order on round-trip, so the model keeps the full mapping verbatim
//! (`serde_yaml::Mapping` is insertion-ordered) and layers typed accessors on
//! top rather than deserializing into a fixed struct.
//!
//! This module is the single point of contact with the YAML library; the rest
//! of the crate depends only on the types exposed here.

use serde_yaml::Value;

/// A parsed frontmatter block, preserving key order and unknown keys.
#[derive(Debug, Clone, Default)]
pub struct Frontmatter {
    map: serde_yaml::Mapping,
}

impl Frontmatter {
    /// Parse a YAML frontmatter block (the text between the `---` delimiters).
    ///
    /// An empty block parses to empty frontmatter. A block whose top level is
    /// not a mapping (e.g. a bare scalar or sequence) is rejected, since OKF
    /// frontmatter is always a set of key/value pairs.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`serde_yaml::Error`] if the block is not valid
    /// YAML or [`NotAMapping`](FrontmatterError::NotAMapping) if it parses to a
    /// non-mapping value.
    pub fn parse(yaml: &str) -> Result<Self, FrontmatterError> {
        if yaml.trim().is_empty() {
            return Ok(Self::default());
        }
        let value: Value = serde_yaml::from_str(yaml).map_err(FrontmatterError::Yaml)?;
        match value {
            Value::Mapping(map) => Ok(Self { map }),
            Value::Null => Ok(Self::default()),
            _ => Err(FrontmatterError::NotAMapping),
        }
    }

    /// Look up a key, returning its value if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }

    /// The value of a key as a string slice, if it is present and a scalar string.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.map.get(key).and_then(Value::as_str)
    }

    /// The required `type` field (SPEC §4.1), if present and non-empty.
    #[must_use]
    pub fn type_(&self) -> Option<&str> {
        self.get_str("type").filter(|s| !s.trim().is_empty())
    }

    /// The recommended `title` field.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.get_str("title")
    }

    /// The recommended `description` field.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.get_str("description")
    }

    /// The recommended `tags` field as a list of strings.
    ///
    /// Non-string entries in the list are skipped; a `tags` value that is not a
    /// list yields an empty vector.
    #[must_use]
    pub fn tags(&self) -> Vec<&str> {
        self.map
            .get("tags")
            .and_then(Value::as_sequence)
            .map(|seq| seq.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }

    /// Every scalar string value in the frontmatter, in key order, including the
    /// elements of any string-valued sequence (e.g. `tags`).
    ///
    /// This is the searchable text of the frontmatter — it intentionally ignores
    /// keys and non-string scalars, since those are rarely what a keyword query
    /// targets. Nested mappings are not descended into.
    #[must_use]
    pub fn string_values(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for (_, value) in &self.map {
            match value {
                Value::String(s) => out.push(s.as_str()),
                Value::Sequence(seq) => out.extend(seq.iter().filter_map(Value::as_str)),
                _ => {}
            }
        }
        out
    }

    /// Set a scalar string `key` to `value`, inserting it or overwriting in place.
    ///
    /// An existing key keeps its position (so round-trips stay stable); a new key
    /// is appended after the current last key.
    pub fn set_str(&mut self, key: &str, value: &str) {
        self.map.insert(
            Value::String(key.to_owned()),
            Value::String(value.to_owned()),
        );
    }

    /// Set the `tags` field to a YAML list of the given strings.
    pub fn set_tags(&mut self, tags: &[String]) {
        let seq = tags.iter().cloned().map(Value::String).collect();
        self.map
            .insert(Value::String("tags".to_owned()), Value::Sequence(seq));
    }

    /// Overlay every key from `other` onto `self`.
    ///
    /// Keys already present are overwritten in place (keeping their position);
    /// keys new to `self` are appended in `other`'s order. Mirrors `set_str`'s
    /// insertion semantics so an explicit flag and a `--frontmatter` overlay
    /// compose predictably.
    pub fn merge(&mut self, other: &Frontmatter) {
        for (key, value) in &other.map {
            self.map.insert(key.clone(), value.clone());
        }
    }

    /// Whether the frontmatter contains no keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The keys in their original order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.map.iter().filter_map(|(k, _)| k.as_str())
    }

    /// Render the frontmatter back to a YAML block, preserving key order.
    ///
    /// Not guaranteed byte-identical to the source (byte-perfect round-tripping
    /// is deferred to mutation, Phase 3); key order is preserved.
    ///
    /// # Errors
    ///
    /// Returns [`FrontmatterError::Serialize`] if the mapping cannot be
    /// serialized to YAML.
    pub fn to_yaml(&self) -> Result<String, FrontmatterError> {
        serde_yaml::to_string(&self.map).map_err(FrontmatterError::Serialize)
    }
}

/// Split raw document content into its leading `---`-delimited frontmatter YAML
/// and the body that follows the closing delimiter.
///
/// The opening `---` must be the very first line. The scan is line-aligned so a
/// `---` appearing inside a YAML value still closes the block correctly. Both
/// `\n` and `\r\n` line endings are accepted. Returns `None` when the content
/// does not open with `---` or the closing delimiter is missing.
#[must_use]
pub fn split_document(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            let yaml = rest.get(..offset).unwrap_or("");
            let body = rest.get(offset + line.len()..).unwrap_or("");
            return Some((yaml, body));
        }
        offset += line.len();
    }
    None
}

impl serde::Serialize for Frontmatter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.map.serialize(serializer)
    }
}

/// A failure while parsing a frontmatter block.
#[derive(Debug, thiserror::Error)]
pub enum FrontmatterError {
    /// The block was not valid YAML.
    #[error("invalid YAML frontmatter: {0}")]
    Yaml(#[source] serde_yaml::Error),

    /// The block parsed to a non-mapping value (scalar or sequence).
    #[error("frontmatter must be a mapping of key/value pairs")]
    NotAMapping,

    /// The mapping could not be serialized back to YAML.
    #[error("could not serialize frontmatter to YAML: {0}")]
    Serialize(#[source] serde_yaml::Error),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn parses_typed_fields() {
        let fm = Frontmatter::parse(
            "type: BigQuery Table\ntitle: Customer Orders\ntags: [sales, orders]",
        )
        .unwrap();
        assert_eq!(fm.type_(), Some("BigQuery Table"));
        assert_eq!(fm.title(), Some("Customer Orders"));
        assert_eq!(fm.tags(), vec!["sales", "orders"]);
        assert_eq!(fm.description(), None);
    }

    #[test]
    fn preserves_key_order_including_unknown_keys() {
        let fm = Frontmatter::parse("type: Metric\nowner: data-team\ntitle: WAU").unwrap();
        let keys: Vec<&str> = fm.keys().collect();
        assert_eq!(keys, vec!["type", "owner", "title"]);
    }

    #[test]
    fn empty_block_is_empty_frontmatter() {
        let fm = Frontmatter::parse("   ").unwrap();
        assert!(fm.is_empty());
        assert_eq!(fm.type_(), None);
    }

    #[test]
    fn blank_type_is_treated_as_absent() {
        let fm = Frontmatter::parse("type: '   '").unwrap();
        assert_eq!(fm.type_(), None);
    }

    #[test]
    fn non_mapping_is_rejected() {
        let err = Frontmatter::parse("- just\n- a\n- list").unwrap_err();
        assert!(matches!(err, FrontmatterError::NotAMapping));
    }

    #[test]
    fn set_str_inserts_new_key_and_overwrites_in_place() {
        let mut fm = Frontmatter::parse("type: Table\ntitle: Old").unwrap();
        fm.set_str("title", "New");
        fm.set_str("owner", "data-team");
        assert_eq!(fm.title(), Some("New"));
        // Overwrite keeps position; the new key is appended last.
        let keys: Vec<&str> = fm.keys().collect();
        assert_eq!(keys, vec!["type", "title", "owner"]);
    }

    #[test]
    fn set_tags_replaces_the_list() {
        let mut fm = Frontmatter::parse("type: Table\ntags: [old]").unwrap();
        fm.set_tags(&["sales".to_owned(), "orders".to_owned()]);
        assert_eq!(fm.tags(), vec!["sales", "orders"]);
    }

    #[test]
    fn merge_overlays_and_preserves_existing_positions() {
        let mut fm = Frontmatter::parse("type: Table\ntitle: Orders").unwrap();
        // A JSON object is valid YAML, so `parse` accepts the `--frontmatter` form.
        let overlay = Frontmatter::parse(r#"{"title": "Customer Orders", "owner": "x"}"#).unwrap();
        fm.merge(&overlay);
        assert_eq!(fm.title(), Some("Customer Orders"));
        let keys: Vec<&str> = fm.keys().collect();
        assert_eq!(keys, vec!["type", "title", "owner"]);
    }
}
