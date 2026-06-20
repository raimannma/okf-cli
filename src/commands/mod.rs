//! Subcommand implementations.
//!
//! Each command lives in its own module and exposes a `run` function that takes
//! its typed arguments plus the [`OutputMode`] and returns
//! [`crate::error::Result`]. Commands are thin: they call into [`crate::core`]
//! and render the result.

mod check;
mod context;
mod fmt;
mod get;
mod index;
mod link;
mod list;
mod log;
mod mutate;
mod mv;
mod neighbors;
mod patch;
mod resolve;
mod rm;
mod search;
mod serve;
mod set;
mod skills;

use std::path::Path;
use std::process::ExitCode;

use crate::cli::{Command, SkillsCommand};
use crate::error::Result;
use crate::output::OutputMode;

/// Dispatch a parsed [`Command`] to its handler, rooted at the bundle directory
/// `root` (resolved from the global `-C` flag).
///
/// Returns the process exit code on success. Most commands always succeed with
/// [`ExitCode::SUCCESS`]; `get` can report a non-zero code when some requested
/// concept could not be served (see [`get::run`]).
///
/// # Errors
///
/// Returns any error produced by the selected subcommand.
// A flat match over every subcommand: it grows one arm per command and does no
// real work itself, so the length lint does not apply.
#[allow(clippy::too_many_lines)]
pub(crate) fn dispatch(command: Command, root: &Path, mode: OutputMode) -> Result<ExitCode> {
    match command {
        Command::List {
            type_,
            tag,
            path_prefix,
            modified_since,
        } => list::run(
            root,
            list::ListArgs {
                types: type_,
                tags: tag,
                path_prefix,
                modified_since,
            },
            mode,
        )
        .map(|()| ExitCode::SUCCESS),
        Command::Search {
            query,
            limit,
            min_score,
        } => {
            search::run(root, &query.join(" "), limit, min_score, mode).map(|()| ExitCode::SUCCESS)
        }
        Command::Get {
            concept_ids,
            frontmatter_only,
            body_only,
            section,
        } => {
            let scope = if let Some(heading) = section {
                get::GetScope::Section(heading)
            } else if frontmatter_only {
                get::GetScope::FrontmatterOnly
            } else if body_only {
                get::GetScope::BodyOnly
            } else {
                get::GetScope::Full
            };
            get::run(root, &concept_ids, &scope, mode)
        }
        Command::Set {
            concept_id,
            type_,
            title,
            description,
            tag,
            frontmatter,
            body,
            dry_run,
            force,
            no_reindex,
        } => set::run(
            root,
            &concept_id,
            &set::SetArgs {
                type_,
                title,
                description,
                tags: tag,
                frontmatter,
                body,
                dry_run,
                force,
                reindex: !no_reindex,
            },
            mode,
        )
        .map(|()| ExitCode::SUCCESS),
        Command::Patch {
            concept_id,
            section,
            content,
            append,
            dry_run,
            force,
        } => patch::run(
            root,
            &concept_id,
            &patch::PatchArgs {
                section,
                content,
                append,
                dry_run,
                force,
            },
            mode,
        )
        .map(|()| ExitCode::SUCCESS),
        Command::Link {
            from,
            to,
            section,
            text,
            dry_run,
            force,
        } => link::link(
            root,
            &from,
            &to,
            &link::LinkArgs {
                section,
                text,
                dry_run,
                force,
            },
            mode,
        )
        .map(|()| ExitCode::SUCCESS),
        Command::Unlink {
            from,
            to,
            dry_run,
            force,
        } => link::unlink(root, &from, &to, &link::UnlinkArgs { dry_run, force }, mode)
            .map(|()| ExitCode::SUCCESS),
        Command::Move {
            old_id,
            new_id,
            dry_run,
            force,
            no_reindex,
        } => mv::run(
            root,
            &old_id,
            &new_id,
            &mv::MvArgs {
                dry_run,
                force,
                reindex: !no_reindex,
            },
            mode,
        )
        .map(|()| ExitCode::SUCCESS),
        Command::Remove {
            concept_ids,
            dry_run,
            force,
            no_reindex,
        } => rm::run(
            root,
            &concept_ids,
            &rm::RmArgs {
                dry_run,
                force,
                reindex: !no_reindex,
            },
            mode,
        )
        .map(|()| ExitCode::SUCCESS),
        Command::Log {
            append: _,
            entry,
            dry_run,
        } => log::run(root, entry.as_deref(), dry_run, mode).map(|()| ExitCode::SUCCESS),
        Command::Fmt {
            concept_ids,
            write,
            force,
        } => fmt::run(root, &concept_ids, &fmt::FmtArgs { write, force }, mode)
            .map(|()| ExitCode::SUCCESS),
        Command::Neighbors { concept_id, depth } => {
            neighbors::run(root, &concept_id, depth, mode).map(|()| ExitCode::SUCCESS)
        }
        Command::Context { concept_id, depth } => {
            context::run(root, &concept_id, depth, mode).map(|()| ExitCode::SUCCESS)
        }
        Command::Resolve { link, from } => {
            resolve::run(root, &link, from.as_deref(), mode).map(|()| ExitCode::SUCCESS)
        }
        Command::Index {
            regenerate: _,
            check,
            dry_run,
        } => index::run(root, &index::IndexArgs { check, dry_run }, mode),
        Command::Check {} => check::run(root, mode),
        Command::Serve { mcp: _ } => serve::run(root),
        Command::Skills { command } => match command {
            SkillsCommand::Install {
                agent,
                dir,
                print,
                dry_run,
                force,
            } => skills::run(
                &skills::InstallArgs {
                    agent,
                    dir,
                    print,
                    dry_run,
                    force,
                },
                mode,
            )
            .map(|()| ExitCode::SUCCESS),
        },
    }
}
