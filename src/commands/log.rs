//! `log --append` subcommand: append a dated entry to the reserved `log.md`.
//!
//! `log.md` (SPEC §7) is the bundle's chronological, append-only change history:
//! `# YYYY-MM-DD` date headings, newest first, with prose entries beneath each.
//! This command adds one entry under today's date — creating that date heading at
//! the top of the file when it is the day's first entry, and creating `log.md`
//! itself when it does not yet exist. Like every Phase 2 mutation it previews with
//! `--dry-run` and reports a unified diff. There is no conformance gate: `log.md`
//! is a reserved file, not a concept with frontmatter to validate.

use std::path::Path;

use time::OffsetDateTime;

use crate::commands::mutate::{read_stdin_if_piped, unified_diff};
use crate::error::{Error, Result};
use crate::output::OutputMode;

/// The reserved change-history filename, appended to at the bundle root (or, with
/// `-C`, at a nested scope).
const LOG_FILENAME: &str = "log.md";

/// Append an entry to the bundle's `log.md` under today's date (or preview it).
///
/// The entry text is `entry` when given, otherwise read from piped standard input.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] when no non-empty entry is supplied,
/// [`Error::ReadStdin`] if reading piped input fails, [`Error::ReadLog`] if an
/// existing `log.md` cannot be read, or [`Error::WriteLog`] on write failure.
pub(crate) fn run(root: &Path, entry: Option<&str>, dry_run: bool, mode: OutputMode) -> Result<()> {
    let entry = resolve_entry(entry)?;
    let path = root.join(LOG_FILENAME);

    let prior = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(Error::ReadLog {
                path: path.clone(),
                source,
            });
        }
    };

    let today = today_utc();
    let (new_log, new_section) = append_entry(&prior, &today, &entry);
    let diff = unified_diff(&prior, &new_log, LOG_FILENAME);

    let written = !dry_run;
    if written {
        std::fs::write(&path, &new_log).map_err(|source| Error::WriteLog {
            path: path.clone(),
            source,
        })?;
    }

    let output = LogOutput {
        path: LOG_FILENAME.to_owned(),
        date: today,
        entry,
        new_section,
        written,
        dry_run,
        diff,
    };
    mode.emit(&output, &output.render_human(), &[]);
    Ok(())
}

/// Resolve the entry text: `--entry`, else piped stdin. An entry is the thing
/// being recorded, so it is required — a blank `--entry ""` or an empty pipe is an
/// input error rather than a silent no-op append.
fn resolve_entry(entry: Option<&str>) -> Result<String> {
    let raw = match entry {
        Some(text) => Some(text.to_owned()),
        None => read_stdin_if_piped()?,
    };
    raw.map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            Error::InvalidInput(
                "no log entry given; pass --entry '…' or pipe it on standard input".to_owned(),
            )
        })
}

/// Today's date as an ISO 8601 `YYYY-MM-DD` string in UTC — the form SPEC §7
/// requires for `log.md` date headings.
fn today_utc() -> String {
    let date = OffsetDateTime::now_utc().date();
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

/// One date-grouped block of the log: its `#` heading line and the prose beneath.
struct Section {
    heading: String,
    body: String,
}

/// Append `entry` under the `today` date heading in `existing`, returning the new
/// log text and whether a new date section was created.
///
/// When `today`'s heading already exists, the entry becomes a new prose paragraph
/// at the end of that day's block. Otherwise a fresh `# today` section is inserted
/// at the top of the file (newest first, SPEC §7). Blank-line spacing is normalized
/// to the canonical one-blank-line-between-blocks form on the way out.
fn append_entry(existing: &str, today: &str, entry: &str) -> (String, bool) {
    let (preamble, mut sections) = parse_sections(existing);

    if let Some(section) = sections
        .iter_mut()
        .find(|s| heading_date(&s.heading) == Some(today))
    {
        let body = section.body.trim();
        section.body = if body.is_empty() {
            entry.to_owned()
        } else {
            format!("{body}\n\n{entry}")
        };
        (render_log(&preamble, &sections), false)
    } else {
        sections.insert(
            0,
            Section {
                heading: format!("# {today}"),
                body: entry.to_owned(),
            },
        );
        (render_log(&preamble, &sections), true)
    }
}

/// Split a log document into any leading non-heading preamble and its `#`-headed
/// sections, each carrying the raw prose beneath its heading.
fn parse_sections(existing: &str) -> (String, Vec<Section>) {
    let mut preamble = String::new();
    let mut sections: Vec<Section> = Vec::new();
    for line in existing.lines() {
        if is_heading(line) {
            sections.push(Section {
                heading: line.trim_end().to_owned(),
                body: String::new(),
            });
        } else if let Some(section) = sections.last_mut() {
            section.body.push_str(line);
            section.body.push('\n');
        } else {
            preamble.push_str(line);
            preamble.push('\n');
        }
    }
    (preamble, sections)
}

/// Re-render a parsed log to canonical form: each section as `heading` + blank line
/// + trimmed body, blocks separated by a single blank line, one trailing newline.
fn render_log(preamble: &str, sections: &[Section]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let preamble = preamble.trim();
    if !preamble.is_empty() {
        blocks.push(preamble.to_owned());
    }
    for section in sections {
        let body = section.body.trim();
        if body.is_empty() {
            blocks.push(section.heading.clone());
        } else {
            blocks.push(format!("{}\n\n{}", section.heading, body));
        }
    }
    let mut out = blocks.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Whether a line is a markdown ATX heading (`#`-led).
fn is_heading(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// The heading's text with its leading `#`s and surrounding whitespace stripped,
/// or `None` for an empty heading. Used to match a date heading like `# 2026-06-20`.
fn heading_date(heading: &str) -> Option<&str> {
    let text = heading.trim_start().trim_start_matches('#').trim();
    (!text.is_empty()).then_some(text)
}

/// The stable JSON contract for `okf log --append`.
#[derive(Debug, serde::Serialize)]
struct LogOutput {
    /// The change-log file path, relative to the bundle root.
    path: String,
    /// The ISO 8601 date heading the entry was filed under (today, UTC).
    date: String,
    /// The entry prose that was appended.
    entry: String,
    /// Whether a new date heading was created (the day's first entry) versus
    /// appended beneath an existing one.
    new_section: bool,
    /// Whether the file was actually written (false for a dry run).
    written: bool,
    /// Whether this invocation was a preview only.
    dry_run: bool,
    /// A unified diff of the change.
    #[serde(skip_serializing_if = "str::is_empty")]
    diff: String,
}

impl LogOutput {
    fn render_human(&self) -> String {
        let placement = if self.new_section {
            format!("under new heading `# {}`", self.date)
        } else {
            format!("under `# {}`", self.date)
        };
        if self.dry_run {
            return format!(
                "[dry run] would append entry to {} {placement}\n{}",
                self.path, self.diff
            );
        }
        format!("appended entry to {} {placement}\n", self.path)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn mode() -> OutputMode {
        OutputMode::test(crate::cli::Format::Text, false, "log")
    }

    #[test]
    fn appends_paragraph_under_existing_date() {
        let existing = "# 2026-06-20\n\n**Creation** Seeded the bundle.\n";
        let (out, new_section) = append_entry(existing, "2026-06-20", "**Update** Added orders.");
        assert!(!new_section);
        assert_eq!(
            out,
            "# 2026-06-20\n\n**Creation** Seeded the bundle.\n\n**Update** Added orders.\n"
        );
    }

    #[test]
    fn creates_new_date_section_at_top() {
        let existing = "# 2026-06-19\n\nOlder entry.\n";
        let (out, new_section) = append_entry(existing, "2026-06-20", "New entry.");
        assert!(new_section);
        assert_eq!(
            out,
            "# 2026-06-20\n\nNew entry.\n\n# 2026-06-19\n\nOlder entry.\n"
        );
    }

    #[test]
    fn creates_log_from_empty() {
        let (out, new_section) = append_entry("", "2026-06-20", "First entry.");
        assert!(new_section);
        assert_eq!(out, "# 2026-06-20\n\nFirst entry.\n");
    }

    #[test]
    fn normalizes_blank_line_spacing() {
        let existing = "# 2026-06-20\n\n\nFirst.\n\n\n";
        let (out, _) = append_entry(existing, "2026-06-20", "Second.");
        assert_eq!(out, "# 2026-06-20\n\nFirst.\n\nSecond.\n");
    }

    #[test]
    fn run_writes_log_file() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), Some("**Update** Did a thing."), false, mode()).unwrap();
        let written = fs::read_to_string(dir.path().join("log.md")).unwrap();
        assert!(written.starts_with("# "));
        assert!(written.contains("**Update** Did a thing."));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = TempDir::new().unwrap();
        run(dir.path(), Some("entry"), true, mode()).unwrap();
        assert!(!dir.path().join("log.md").exists());
    }

    #[test]
    fn empty_entry_is_an_input_error() {
        let dir = TempDir::new().unwrap();
        let err = run(dir.path(), Some("   "), false, mode()).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn missing_entry_with_no_pipe_is_an_input_error() {
        let dir = TempDir::new().unwrap();
        // stdin is the test harness's terminal-less pipe; with no --entry the
        // command must refuse rather than append a blank line.
        let err = run(dir.path(), None, false, mode()).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }
}
