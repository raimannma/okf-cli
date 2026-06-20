//! Error types for the application.
//!
//! Every fallible operation returns [`Error`]. Variants carry enough context to
//! render an actionable, user-facing message via their `Display` implementation,
//! so the top-level handler can print them directly without leaking internals.

use std::path::PathBuf;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// All errors the application can surface to the user.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A required file could not be read.
    #[error("could not read file `{path}`: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// User-supplied input failed validation.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// The given bundle path is not an existing directory.
    #[error("bundle path `{path}` is not a directory")]
    BundleNotADirectory { path: PathBuf },

    /// A `--modified-since` value was not a recognized date or datetime.
    #[error(
        "invalid --modified-since value `{value}`: expected an RFC 3339 datetime \
         (e.g. 2026-01-15T10:30:00Z) or a YYYY-MM-DD date"
    )]
    InvalidModifiedSince { value: String },

    /// No concept with the requested ID exists in the bundle.
    #[error("concept `{id}` not found in bundle")]
    ConceptNotFound { id: String },

    /// The requested section heading was not found in the concept's body.
    #[error("section `{section}` not found in concept `{id}`")]
    SectionNotFound { section: String, id: String },

    /// A concept's frontmatter could not be rendered back to YAML.
    #[error("could not render frontmatter for concept `{id}` as YAML")]
    RenderFrontmatter {
        id: String,
        #[source]
        source: crate::core::frontmatter::FrontmatterError,
    },

    /// A concept could not be written to disk.
    #[error("could not write concept `{id}` to `{path}`: {source}")]
    WriteFile {
        id: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The body could not be read from standard input.
    #[error("could not read concept body from standard input: {source}")]
    ReadStdin {
        #[source]
        source: std::io::Error,
    },

    /// The `--frontmatter` argument was not a valid JSON/YAML mapping.
    #[error("invalid --frontmatter value: {source}")]
    InvalidFrontmatterArg {
        #[source]
        source: crate::core::frontmatter::FrontmatterError,
    },

    /// A mutation would produce a concept that fails OKF conformance.
    #[error("concept `{id}` is not conformant: {reason}")]
    NotConformant { id: String, reason: String },

    /// An existing concept file could not be parsed, so it cannot be patched.
    #[error("could not parse concept `{id}` for editing: {source}")]
    ParseConcept {
        id: String,
        #[source]
        source: crate::core::concept::ConceptParseError,
    },

    /// The target concept ID maps to a reserved filename (`index.md`/`log.md`).
    #[error(
        "`{id}` maps to a reserved filename (index.md/log.md); these are not \
         concepts and cannot be written with `set`"
    )]
    ReservedConcept { id: String },

    /// A `mv` destination already names an existing concept; refusing to clobber it.
    #[error(
        "destination concept `{id}` already exists; choose a new id or remove it first \
         (mv never overwrites)"
    )]
    ConceptExists { id: String },

    /// The reserved `log.md` change history could not be read for appending.
    #[error("could not read change log `{path}`: {source}")]
    ReadLog {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The reserved `log.md` change history could not be written.
    #[error("could not append to change log `{path}`: {source}")]
    WriteLog {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// An existing reserved `index.md` listing could not be read for regeneration.
    #[error("could not read index `{path}`: {source}")]
    ReadIndex {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A regenerated reserved `index.md` listing could not be written or removed.
    #[error("could not write index `{path}`: {source}")]
    WriteIndex {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Output could not be serialized to JSON.
    #[error("could not render JSON output")]
    Json(#[source] serde_json::Error),

    /// The MCP server could not read a request or write a response on stdio.
    #[error("MCP server stdio failure: {source}")]
    McpIo {
        #[source]
        source: std::io::Error,
    },

    /// No home directory could be found to derive the default skill location.
    #[error(
        "could not determine a home directory for the default skill location; \
         pass --dir <path> to choose where to install the skill"
    )]
    NoHomeDir,

    /// A skill file already exists and `--force` was not given.
    #[error("skill file `{path}` already exists; pass --force to overwrite it")]
    SkillExists { path: PathBuf },

    /// The generated skill file, or its directory, could not be written.
    #[error("could not write skill to `{path}`: {source}")]
    WriteSkill {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    /// The process exit code to use when this error reaches `main`.
    ///
    /// Distinct codes let scripts branch on failure class.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::InvalidInput(_)
            | Error::InvalidModifiedSince { .. }
            | Error::InvalidFrontmatterArg { .. }
            | Error::ReservedConcept { .. }
            | Error::ConceptExists { .. }
            | Error::SkillExists { .. } => 2,
            Error::ConceptNotFound { .. } | Error::SectionNotFound { .. } => 3,
            Error::NotConformant { .. } => 4,
            Error::ReadFile { .. }
            | Error::BundleNotADirectory { .. }
            | Error::RenderFrontmatter { .. }
            | Error::WriteFile { .. }
            | Error::ReadStdin { .. }
            | Error::ParseConcept { .. }
            | Error::ReadLog { .. }
            | Error::WriteLog { .. }
            | Error::ReadIndex { .. }
            | Error::WriteIndex { .. }
            | Error::Json(_)
            | Error::McpIo { .. }
            | Error::NoHomeDir
            | Error::WriteSkill { .. } => 1,
        }
    }
}
