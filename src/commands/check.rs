//! `check` subcommand: validate the whole bundle without changing anything.
//!
//! The read-only counterpart to the mutating commands' per-edit checks: it loads
//! every concept and reports the problems a bundle can accumulate over many hand
//! or agent edits — links that resolve to a concept absent from the bundle
//! (broken links, legal under SPEC §5 but worth surfacing) and files that could
//! not be parsed as concepts. Nothing is written. It exits non-zero (code 5) when
//! any problem is found, so CI can gate on a clean bundle, mirroring `index
//! --check`.

use std::path::Path;
use std::process::ExitCode;

use crate::core::Bundle;
use crate::core::check::broken_links;
use crate::error::Result;
use crate::output::OutputMode;

/// Exit code when `check` finds at least one problem (distinct from a hard error).
const ISSUES_EXIT_CODE: u8 = 5;

/// Check the bundle under `root` and report any problems.
///
/// Returns [`ExitCode::SUCCESS`] for a clean bundle, or [`ISSUES_EXIT_CODE`] when
/// any broken link or unparseable file is found.
///
/// # Errors
///
/// [`Error::BundleNotADirectory`](crate::error::Error::BundleNotADirectory) if
/// `root` is not a directory.
pub(crate) fn run(root: &Path, mode: OutputMode) -> Result<ExitCode> {
    let bundle = Bundle::load(root)?;

    let broken: Vec<BrokenLinkReport> = broken_links(&bundle)
        .into_iter()
        .map(|b| BrokenLinkReport {
            from: b.from.to_string(),
            target: b.target.to_string(),
            raw: b.raw,
        })
        .collect();

    let unparseable: Vec<UnparseableReport> = bundle
        .parse_errors()
        .iter()
        .map(|e| UnparseableReport {
            path: e.path.display().to_string(),
            reason: e.reason(),
        })
        .collect();

    let issues = broken.len() + unparseable.len();
    let output = CheckOutput {
        ok: issues == 0,
        concepts: bundle.len(),
        issues,
        broken_links: broken,
        unparseable,
    };
    mode.emit(&output, &output.render_human(), &[]);

    Ok(if output.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(ISSUES_EXIT_CODE)
    })
}

/// One broken outbound link in the bundle.
#[derive(Debug, serde::Serialize)]
struct BrokenLinkReport {
    /// The concept the link is written in.
    from: String,
    /// The concept ID it resolves to but which does not exist in the bundle.
    target: String,
    /// The link destination exactly as written.
    raw: String,
}

/// One file that could not be loaded as a concept.
#[derive(Debug, serde::Serialize)]
struct UnparseableReport {
    /// The file's bundle-relative path.
    path: String,
    /// Why it could not be loaded.
    reason: String,
}

/// The stable JSON contract for `okf check`.
#[derive(Debug, serde::Serialize)]
struct CheckOutput {
    /// Whether the bundle is clean (no problems found).
    ok: bool,
    /// How many concepts were loaded and checked.
    concepts: usize,
    /// Total number of problems found.
    issues: usize,
    /// Each broken outbound link, ordered by source concept.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    broken_links: Vec<BrokenLinkReport>,
    /// Each file that could not be parsed as a concept.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unparseable: Vec<UnparseableReport>,
}

impl CheckOutput {
    fn render_human(&self) -> String {
        use std::fmt::Write as _;

        if self.ok {
            return format!("no problems found in {} concept(s)\n", self.concepts);
        }

        let mut out = String::new();
        let _ = writeln!(
            out,
            "found {} problem(s) across {} concept(s)",
            self.issues, self.concepts
        );
        for b in &self.broken_links {
            let _ = writeln!(
                out,
                "  broken link: {} → {} (written as `{}`)",
                b.from, b.target, b.raw
            );
        }
        for u in &self.unparseable {
            let _ = writeln!(out, "  unparseable: {} ({})", u.path, u.reason);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn mode() -> OutputMode {
        OutputMode::test(crate::cli::Format::Text, false, "check")
    }

    fn write(dir: &TempDir, rel: &str, content: &str) {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn clean_bundle_exits_zero() {
        let dir = TempDir::new().unwrap();
        write(&dir, "a.md", "---\ntype: T\n---\nlinks [b](/b.md)\n");
        write(&dir, "b.md", "---\ntype: T\n---\nno links\n");
        assert_eq!(run(dir.path(), mode()).unwrap(), ExitCode::SUCCESS);
    }

    #[test]
    fn broken_link_exits_with_issue_code() {
        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "a.md",
            "---\ntype: T\n---\nlinks [ghost](/ghost.md)\n",
        );
        assert_eq!(
            run(dir.path(), mode()).unwrap(),
            ExitCode::from(ISSUES_EXIT_CODE)
        );
    }

    #[test]
    fn unparseable_file_exits_with_issue_code() {
        let dir = TempDir::new().unwrap();
        write(&dir, "good.md", "---\ntype: T\n---\nok\n");
        write(&dir, "bad.md", "no frontmatter here\n");
        assert_eq!(
            run(dir.path(), mode()).unwrap(),
            ExitCode::from(ISSUES_EXIT_CODE)
        );
    }
}
