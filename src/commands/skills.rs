//! `skills install` subcommand: generate and install an agent skill for okf.
//!
//! The skill body is assembled at run time from the live clap command model
//! (`Cli::command()`), so it can never drift from the real CLI: the moment a
//! command or flag is added, a freshly generated skill lists it. Both supported
//! agents — Claude and Codex — consume the same [Agent Skills] layout (a
//! directory named after the skill, holding a `SKILL.md` with `name` +
//! `description` frontmatter), so the only per-agent difference is the default
//! install directory.
//!
//! [Agent Skills]: https://agentskills.io/specification

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::{Arg, ArgAction, Command, CommandFactory};

use crate::cli::{Cli, SkillAgent};
use crate::error::{Error, Result};
use crate::output::OutputMode;

/// The skill's name. Per the Agent Skills spec the parent directory must match
/// it, so the skill always lands in `<skills-dir>/okf/SKILL.md`.
const SKILL_NAME: &str = "okf";

/// The fixed filename every Agent Skills implementation looks for.
const SKILL_FILENAME: &str = "SKILL.md";

/// The frontmatter `description` — the field agents use to decide when to load
/// the skill. Kept well under the spec's 1024-character limit.
const DESCRIPTION: &str = "Drive the okf CLI to work with Open Knowledge Format (OKF) bundles: \
survey concepts, retrieve them (whole, or just one section or the frontmatter), \
search, traverse the link graph, and safely create/edit/move/remove concepts \
with diff previews and conformance gating. Use whenever the user works with an \
OKF bundle (concepts as markdown files with YAML frontmatter, reserved index.md \
and log.md, cross-linked via markdown links), mentions okf, or needs to assemble \
context from a local knowledge base.";

/// Parsed arguments for `skills install`.
#[derive(Debug)]
pub(crate) struct InstallArgs {
    pub agent: SkillAgent,
    pub dir: Option<PathBuf>,
    pub print: bool,
    pub dry_run: bool,
    pub force: bool,
}

impl SkillAgent {
    /// The agent's conventional skills directory (the parent of the `okf/`
    /// skill directory), or `None` when no home directory can be found.
    fn default_skills_dir(self) -> Option<PathBuf> {
        #[allow(deprecated)]
        let home = std::env::home_dir()?;
        Some(match self {
            SkillAgent::Claude => home.join(".claude").join("skills"),
            SkillAgent::Codex => home.join(".agents").join("skills"),
        })
    }

    /// A short, stable label for the JSON contract and human messages.
    fn label(self) -> &'static str {
        match self {
            SkillAgent::Claude => "claude",
            SkillAgent::Codex => "codex",
        }
    }
}

/// Generate the skill for `args.agent` and install it (or preview/print it).
///
/// # Errors
///
/// Returns [`Error::NoHomeDir`] when the default location is needed but no home
/// directory exists, [`Error::SkillExists`] when the target file is present and
/// `--force` was not given, or [`Error::WriteSkill`] on a filesystem failure.
pub(crate) fn run(args: &InstallArgs, mode: OutputMode) -> Result<()> {
    let content = render_skill(args.agent);

    let skills_dir = match &args.dir {
        Some(dir) => dir.clone(),
        None => args.agent.default_skills_dir().ok_or(Error::NoHomeDir)?,
    };
    let target_dir = skills_dir.join(SKILL_NAME);
    let path = target_dir.join(SKILL_FILENAME);
    let path_str = path.display().to_string();

    if args.print {
        let out = SkillOutput::preview(args.agent, path_str, &content);
        mode.emit(&out, &content, &[]);
        return Ok(());
    }

    if args.dry_run {
        let human = format!(
            "[dry run] would install {}-agent skill ({} bytes) to {path_str}\n\n{content}",
            args.agent.label(),
            content.len(),
        );
        let out = SkillOutput::preview(args.agent, path_str, &content);
        mode.emit(&out, &human, &[]);
        return Ok(());
    }

    if path.exists() && !args.force {
        return Err(Error::SkillExists { path });
    }
    std::fs::create_dir_all(&target_dir).map_err(|source| Error::WriteSkill {
        path: target_dir.clone(),
        source,
    })?;
    std::fs::write(&path, &content).map_err(|source| Error::WriteSkill {
        path: path.clone(),
        source,
    })?;

    let human = format!(
        "installed {}-agent skill ({} bytes) to {path_str}\n",
        args.agent.label(),
        content.len(),
    );
    let out = SkillOutput::written(args.agent, path_str);
    mode.emit(&out, &human, &[]);
    Ok(())
}

/// Build the full `SKILL.md` text: spec frontmatter plus a body derived from the
/// live clap model.
fn render_skill(_agent: SkillAgent) -> String {
    let cmd = Cli::command();
    let mut out = String::new();
    out.push_str("---\n");
    let _ = writeln!(out, "name: {SKILL_NAME}");
    let _ = writeln!(out, "description: {}", yaml_quote(DESCRIPTION));
    out.push_str("---\n\n");
    render_body(&cmd, &mut out);
    out
}

/// Render the markdown body: overview, output contract, global flags, the full
/// command/flag listing (from clap), and worked examples.
fn render_body(cmd: &Command, out: &mut String) {
    out.push_str(
        "# okf — agent-oriented CLI for OKF bundles\n\n\
         `okf` reads from and writes to a local [Open Knowledge Format](https://github.com/GoogleCloudPlatform/knowledge-catalog/tree/main/okf) \
         bundle: a tree of *concepts* (markdown files with YAML frontmatter) \
         cross-linked with markdown links, plus reserved `index.md` listings and \
         a `log.md` change history. Every command is built for the agent loop: \
         **survey → retrieve → assemble → mutate → validate**, paying only for \
         what each step needs.\n\n\
         Run commands from inside the bundle, or point at it with `-C <dir>` \
         (or the `OKF_DIR` environment variable). Concept IDs are bundle-relative \
         paths without the `.md` suffix (e.g. `tables/orders`).\n\n",
    );

    render_output_contract(out);
    render_global_flags(cmd, out);
    render_commands(cmd, out);
    render_examples(out);
}

/// The fixed parts of the contract: the `--format` envelope and exit codes.
fn render_output_contract(out: &mut String) {
    out.push_str(
        "## Output contract\n\n\
         Pass `--format json` (alias `-o json`) to get one machine-readable \
         envelope per call: `{\"command\", \"ok\", \"data\", \"warnings\"}` on \
         success, or `{\"command\", \"ok\": false, \"error\", \"warnings\"}` on \
         failure. The default `--format text` is self-describing human output. \
         Results go to stdout; warnings and errors go to stderr.\n\n\
         Exit codes let a harness branch on failure class: `0` success, \
         `2` bad input/usage, `3` concept or section not found, `4` mutation \
         blocked by a conformance failure, `5` problems found by a read-only \
         check (`index --check` drift, `check` broken links), \
         `1` any other I/O or internal error.\n\n",
    );
}

/// Render the global flags shared by every command (from the top-level args
/// marked `global`, skipping hidden ones like the deprecated `--json`).
fn render_global_flags(cmd: &Command, out: &mut String) {
    out.push_str("## Global flags\n\n");
    for arg in cmd.get_arguments() {
        if arg.is_global_set() && !arg.is_hide_set() {
            render_arg_line(arg, out);
        }
    }
    out.push('\n');
}

/// Render the command listing, one `###` block per (sub)command, recursing into
/// nested subcommands like `skills install`.
fn render_commands(cmd: &Command, out: &mut String) {
    out.push_str("## Commands\n\n");
    for sub in cmd.get_subcommands() {
        if !sub.is_hide_set() {
            render_command(sub, "okf", out);
        }
    }
}

/// Render a single command: its `okf <path>` heading, one-line description, and
/// each of its own (non-global, non-hidden) arguments, then any subcommands.
fn render_command(sub: &Command, prefix: &str, out: &mut String) {
    let path = format!("{prefix} {}", sub.get_name());
    let _ = write!(out, "### `{path}`");
    if let Some(about) = sub.get_about() {
        let _ = write!(out, " — {}", first_line(&about.to_string()));
    }
    out.push_str("\n\n");

    for arg in sub.get_arguments() {
        if !arg.is_global_set() && !arg.is_hide_set() && !is_builtin(arg) {
            render_arg_line(arg, out);
        }
    }
    out.push('\n');

    for child in sub.get_subcommands() {
        if !child.is_hide_set() {
            render_command(child, &path, out);
        }
    }
}

/// Render one argument as a markdown bullet: `` - `<usage>` — help ``.
fn render_arg_line(arg: &Arg, out: &mut String) {
    let _ = write!(out, "- `{}`", arg_usage(arg));
    let mut help = arg.get_help().map(|h| first_line(&h.to_string()));
    // Only value-taking flags have meaningful choices; boolean flags report a
    // useless `true, false`, so restrict the hint to args that consume a value.
    if takes_value(arg)
        && let Some(values) = possible_values(arg)
    {
        let suffix = format!("Values: {values}.");
        help = Some(match help {
            Some(text) => format!("{text} {suffix}"),
            None => suffix,
        });
    }
    if let Some(text) = help {
        let _ = write!(out, " — {text}");
    }
    out.push('\n');
}

/// The invocation form of an argument: a positional value name (with `...` when
/// it is repeatable) or the option's flags plus its value placeholder.
fn arg_usage(arg: &Arg) -> String {
    let values = value_placeholder(arg);
    if arg.is_positional() {
        let mut usage = values.unwrap_or_else(|| format!("<{}>", arg.get_id()));
        if is_multi(arg) {
            usage.push_str("...");
        }
        return usage;
    }

    let mut flags = Vec::new();
    if let Some(short) = arg.get_short() {
        flags.push(format!("-{short}"));
    }
    if let Some(long) = arg.get_long() {
        flags.push(format!("--{long}"));
    }
    let mut usage = flags.join(", ");
    if takes_value(arg)
        && let Some(values) = values
    {
        let _ = write!(usage, " {values}");
    }
    usage
}

/// The `<NAME>`-style value placeholder(s) declared for an argument, if any.
fn value_placeholder(arg: &Arg) -> Option<String> {
    let names = arg.get_value_names()?;
    if names.is_empty() {
        return None;
    }
    Some(
        names
            .iter()
            .map(|name| format!("<{name}>"))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The comma-joined list of an argument's accepted values (for value-enum flags
/// like `--format` and `--agent`), or `None` when it is unconstrained.
fn possible_values(arg: &Arg) -> Option<String> {
    let values: Vec<String> = arg
        .get_possible_values()
        .iter()
        .map(|value| value.get_name().to_owned())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

/// Whether the argument consumes a value (as opposed to a boolean/count flag).
fn takes_value(arg: &Arg) -> bool {
    matches!(arg.get_action(), ArgAction::Set | ArgAction::Append)
}

/// Whether the argument accepts more than one value.
fn is_multi(arg: &Arg) -> bool {
    arg.get_num_args()
        .is_some_and(|range| range.max_values() > 1)
}

/// Whether this is one of clap's auto-generated `--help` / `--version` args,
/// which are noise in a generated reference.
fn is_builtin(arg: &Arg) -> bool {
    matches!(arg.get_id().as_str(), "help" | "version")
}

/// Worked end-to-end examples of the agent loop, referencing real commands.
fn render_examples(out: &mut String) {
    out.push_str(
        "## Worked examples\n\n\
         Survey, then drill into just what you need:\n\n\
         ```\n\
         okf list --type table                       # cheap survey, IDs + frontmatter only\n\
         okf get tables/orders --section \"# Schema\"   # one section, not the whole file\n\
         ```\n\n\
         Search and walk the graph:\n\n\
         ```\n\
         okf search \"active users\" --limit 5\n\
         okf neighbors tables/orders --depth 2       # outbound links + backlinks\n\
         ```\n\n\
         Mutate safely — always preview with `--dry-run` first, then write:\n\n\
         ```\n\
         okf set facts/refunds --type fact --title Refunds --body \"...\" --dry-run\n\
         okf set facts/refunds --type fact --title Refunds --body \"...\"\n\
         echo \"text\" | okf patch facts/refunds --section \"# Notes\"   # body via stdin\n\
         okf link facts/refunds tables/orders        # correct bundle-relative link\n\
         okf move tables/orders facts/orders --dry-run  # rewrites every inbound link\n\
         ```\n\n\
         Machine-readable output for a harness:\n\n\
         ```\n\
         okf list --format json\n\
         ```\n",
    );
}

/// The first line of a (possibly multi-line) help string, trimmed.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_owned()
}

/// Quote a string as a YAML double-quoted scalar so colons and other special
/// characters in the description survive a YAML parse.
fn yaml_quote(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// The stable JSON contract for `okf skills install`.
#[derive(Debug, serde::Serialize)]
struct SkillOutput {
    /// The target agent (`claude` or `codex`).
    agent: &'static str,
    /// The skill file path, `<skills-dir>/okf/SKILL.md`.
    path: String,
    /// Whether the file was actually written (false for `--print`/`--dry-run`).
    written: bool,
    /// The generated skill content (included only for previews).
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

impl SkillOutput {
    fn written(agent: SkillAgent, path: String) -> Self {
        Self {
            agent: agent.label(),
            path,
            written: true,
            content: None,
        }
    }

    fn preview(agent: SkillAgent, path: String, content: &str) -> Self {
        Self {
            agent: agent.label(),
            path,
            written: false,
            content: Some(content.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn mode() -> OutputMode {
        OutputMode::test(crate::cli::Format::Text, true, "skills")
    }

    fn install_args(dir: &std::path::Path) -> InstallArgs {
        InstallArgs {
            agent: SkillAgent::Claude,
            dir: Some(dir.to_path_buf()),
            print: false,
            dry_run: false,
            force: false,
        }
    }

    #[test]
    fn skill_has_spec_frontmatter() {
        let skill = render_skill(SkillAgent::Claude);
        assert!(skill.starts_with("---\nname: okf\n"));
        assert!(skill.contains("description: \""));
        // Frontmatter is closed before the body heading.
        let body = skill.split("---\n\n").nth(1).unwrap();
        assert!(body.starts_with("# okf"));
    }

    #[test]
    fn skill_lists_every_command_from_clap() {
        // The exit criterion: the listing is derived from the live model, so
        // every subcommand the CLI defines must appear in a generated skill.
        let skill = render_skill(SkillAgent::Claude);
        for sub in Cli::command().get_subcommands() {
            if sub.is_hide_set() {
                continue;
            }
            let heading = format!("### `okf {}`", sub.get_name());
            assert!(
                skill.contains(&heading),
                "generated skill is missing command `{}`",
                sub.get_name()
            );
        }
    }

    #[test]
    fn skill_lists_nested_subcommands_and_flags() {
        let skill = render_skill(SkillAgent::Claude);
        assert!(skill.contains("### `okf skills install`"));
        // A representative flag rendered from clap metadata.
        assert!(skill.contains("--frontmatter-only"));
        assert!(skill.contains("`-C <PATH>`") || skill.contains("-C"));
    }

    #[test]
    fn install_writes_skill_to_okf_subdir() {
        let dir = TempDir::new().unwrap();
        run(&install_args(dir.path()), mode()).unwrap();
        let path = dir.path().join("okf").join("SKILL.md");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("---\nname: okf\n"));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let args = InstallArgs {
            dry_run: true,
            ..install_args(dir.path())
        };
        run(&args, mode()).unwrap();
        assert!(!dir.path().join("okf").exists());
    }

    #[test]
    fn print_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let args = InstallArgs {
            print: true,
            ..install_args(dir.path())
        };
        run(&args, mode()).unwrap();
        assert!(!dir.path().join("okf").exists());
    }

    #[test]
    fn existing_skill_without_force_errors() {
        let dir = TempDir::new().unwrap();
        run(&install_args(dir.path()), mode()).unwrap();
        let err = run(&install_args(dir.path()), mode()).unwrap_err();
        assert!(matches!(err, Error::SkillExists { .. }));
    }

    #[test]
    fn force_overwrites_existing_skill() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("okf").join("SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "stale").unwrap();
        let args = InstallArgs {
            force: true,
            ..install_args(dir.path())
        };
        run(&args, mode()).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("---\nname: okf\n"));
    }

    #[test]
    fn codex_and_claude_default_dirs_differ() {
        // Only meaningful when a home directory exists.
        if let (Some(claude), Some(codex)) = (
            SkillAgent::Claude.default_skills_dir(),
            SkillAgent::Codex.default_skills_dir(),
        ) {
            assert!(claude.ends_with("skills"));
            assert_ne!(claude, codex);
            assert!(codex.to_string_lossy().contains(".agents"));
        }
    }
}
