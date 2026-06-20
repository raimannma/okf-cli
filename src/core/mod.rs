//! The reusable OKF core: parsing, the concept model, and bundle loading.
//!
//! All OKF domain logic lives here so that front-ends — the CLI today, an MCP
//! server later — are thin adapters over one library. Nothing in this module
//! knows about argument parsing or terminal output.

pub mod bundle;
pub mod check;
pub mod concept;
pub mod concept_id;
pub mod frontmatter;
pub mod index;

pub use bundle::Bundle;
pub use concept::Concept;
pub use concept_id::ConceptId;
pub use frontmatter::Frontmatter;
