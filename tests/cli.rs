//! End-to-end tests over the compiled `okf` binary, focused on the process
//! contract a harness depends on: exit codes and the `OKF_DIR` fallback.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Run the binary with `args` (plus `OKF_DIR` if `env_dir` is set) and return its
/// exit code.
fn run(args: &[&str], env_dir: Option<&Path>) -> i32 {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_okf"));
    cmd.args(args);
    if let Some(dir) = env_dir {
        cmd.env("OKF_DIR", dir);
    }
    cmd.output().unwrap().status.code().unwrap()
}

#[test]
fn get_existing_concept_exits_zero() {
    let ga4 = fixture("reference/ga4");
    let code = run(
        &[
            "-C",
            ga4.to_str().unwrap(),
            "get",
            "references/metrics/day_count",
            "--frontmatter-only",
        ],
        None,
    );
    assert_eq!(code, 0);
}

#[test]
fn get_missing_concept_exits_nonzero() {
    let ga4 = fixture("reference/ga4");
    let code = run(&["-C", ga4.to_str().unwrap(), "get", "tables/ghost"], None);
    assert_eq!(code, 3, "a not-found concept must fail the command");
}

#[test]
fn get_partial_failure_still_exits_nonzero() {
    // One id resolves, one does not: the resolved concept is printed, but the
    // command still fails so a harness notices the missing one.
    let ga4 = fixture("reference/ga4");
    let code = run(
        &[
            "-C",
            ga4.to_str().unwrap(),
            "get",
            "references/metrics/day_count",
            "tables/ghost",
            "--frontmatter-only",
        ],
        None,
    );
    assert_eq!(code, 3);
}

#[test]
fn get_missing_section_exits_nonzero() {
    let ga4 = fixture("reference/ga4");
    let code = run(
        &[
            "-C",
            ga4.to_str().unwrap(),
            "get",
            "references/metrics/day_count",
            "--section",
            "# Nope",
        ],
        None,
    );
    assert_eq!(code, 3);
}

#[test]
fn resolve_missing_target_exits_zero() {
    // Broken links are first-class in OKF: resolving one is not a failure.
    let ga4 = fixture("reference/ga4");
    let code = run(
        &["-C", ga4.to_str().unwrap(), "resolve", "/tables/ghost.md"],
        None,
    );
    assert_eq!(code, 0);
}

#[test]
fn okf_dir_env_sets_the_bundle() {
    // No `-C`: the bundle comes from OKF_DIR, and the command succeeds.
    let ga4 = fixture("reference/ga4");
    let code = run(&["list"], Some(&ga4));
    assert_eq!(code, 0);
}

#[test]
fn set_creates_concept_in_a_fresh_bundle() {
    // `set` does not require an existing bundle dir — it creates the tree.
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("bundle");
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "set",
            "tables/orders",
            "--type",
            "Table",
            "--body",
            "# Schema",
        ],
        None,
    );
    assert_eq!(code, 0);
    let written = std::fs::read_to_string(root.join("tables/orders.md")).unwrap();
    assert_eq!(written, "---\ntype: Table\n---\n# Schema\n");
}

#[test]
fn set_without_type_is_gated_and_dry_run_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    // The conformance gate fails with the dedicated exit code 4.
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "set",
            "notes/x",
            "--title",
            "X",
        ],
        None,
    );
    assert_eq!(code, 4, "a non-conformant write must fail the command");
    assert!(!root.join("notes/x.md").exists());

    // `--dry-run` previews a valid write but leaves the disk untouched.
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "set",
            "notes/x",
            "--type",
            "Note",
            "--dry-run",
        ],
        None,
    );
    assert_eq!(code, 0);
    assert!(!root.join("notes/x.md").exists());
}

#[test]
fn patch_creates_section_and_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("tables")).unwrap();
    std::fs::write(
        root.join("tables/orders.md"),
        "---\ntype: Table\n---\n# Schema\n\n- id\n",
    )
    .unwrap();

    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "patch",
            "tables/orders",
            "--section",
            "# Joins",
            "--content",
            "orders.id = items.order_id",
        ],
        None,
    );
    assert_eq!(code, 0);
    let written = std::fs::read_to_string(root.join("tables/orders.md")).unwrap();
    assert_eq!(
        written,
        "---\ntype: Table\n---\n# Schema\n\n- id\n\n# Joins\n\norders.id = items.order_id\n"
    );

    // Re-running the same patch changes nothing and still succeeds.
    let before = std::fs::metadata(root.join("tables/orders.md"))
        .unwrap()
        .modified()
        .unwrap();
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "patch",
            "tables/orders",
            "--section",
            "# Joins",
            "--content",
            "orders.id = items.order_id",
        ],
        None,
    );
    assert_eq!(code, 0);
    let after = std::fs::metadata(root.join("tables/orders.md"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        before, after,
        "an unchanged patch must not rewrite the file"
    );
}

#[test]
fn log_append_creates_and_grows_change_history() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    // First entry creates log.md with today's date heading.
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "log",
            "--append",
            "--entry",
            "**Creation** Seeded the bundle.",
        ],
        None,
    );
    assert_eq!(code, 0);
    let first = std::fs::read_to_string(root.join("log.md")).unwrap();
    assert!(first.starts_with("# "), "log opens with a date heading");
    assert!(first.contains("**Creation** Seeded the bundle."));

    // A second same-day entry is filed under the same heading (one heading only).
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "log",
            "--append",
            "--entry",
            "**Update** Added orders.",
        ],
        None,
    );
    assert_eq!(code, 0);
    let second = std::fs::read_to_string(root.join("log.md")).unwrap();
    assert_eq!(second.matches("# ").count(), 1, "still one date heading");
    assert!(second.contains("**Creation** Seeded the bundle."));
    assert!(second.contains("**Update** Added orders."));
}

#[test]
fn log_append_requires_the_action_flag() {
    // `log --entry …` without `--append` is a clap usage error (exit 2), so a
    // future `log` mode can be added without silently changing this one.
    let dir = TempDir::new().unwrap();
    let code = run(
        &["-C", dir.path().to_str().unwrap(), "log", "--entry", "x"],
        None,
    );
    assert_eq!(code, 2);
    assert!(!dir.path().join("log.md").exists());
}

#[test]
fn patch_missing_concept_exits_not_found() {
    let dir = TempDir::new().unwrap();
    let code = run(
        &[
            "-C",
            dir.path().to_str().unwrap(),
            "patch",
            "tables/ghost",
            "--section",
            "# Joins",
            "--content",
            "x",
        ],
        None,
    );
    assert_eq!(
        code, 3,
        "patching a nonexistent concept must fail as not-found"
    );
}

#[test]
fn link_and_unlink_round_trip_through_the_binary() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("tables")).unwrap();
    std::fs::write(
        root.join("tables/orders.md"),
        "---\ntype: Table\n---\n# Overview\n\norders\n",
    )
    .unwrap();
    std::fs::write(
        root.join("tables/customers.md"),
        "---\ntype: Table\ntitle: Customers\n---\n# Overview\n\ncustomers\n",
    )
    .unwrap();

    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "link",
            "tables/orders",
            "tables/customers",
        ],
        None,
    );
    assert_eq!(code, 0);
    let written = std::fs::read_to_string(root.join("tables/orders.md")).unwrap();
    assert!(
        written.contains("# Related\n\n- [Customers](/tables/customers.md)\n"),
        "link must add a bullet using the target title: {written}"
    );

    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "unlink",
            "tables/orders",
            "tables/customers",
        ],
        None,
    );
    assert_eq!(code, 0);
    let written = std::fs::read_to_string(root.join("tables/orders.md")).unwrap();
    assert!(
        !written.contains("/tables/customers.md"),
        "unlink must remove the bullet: {written}"
    );
}

#[test]
fn link_missing_source_exits_not_found() {
    let dir = TempDir::new().unwrap();
    let code = run(
        &[
            "-C",
            dir.path().to_str().unwrap(),
            "link",
            "tables/ghost",
            "tables/customers",
        ],
        None,
    );
    assert_eq!(code, 3, "linking from a nonexistent concept is not-found");
}

#[test]
fn link_non_conformant_source_is_gated() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("tables")).unwrap();
    std::fs::write(
        root.join("tables/orphan.md"),
        "---\ntitle: Orphan\n---\n# Overview\n\nno type\n",
    )
    .unwrap();
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "link",
            "tables/orphan",
            "tables/customers",
        ],
        None,
    );
    assert_eq!(code, 4, "a non-conformant source is gated without --force");
}

#[test]
fn mv_moves_concept_and_rewrites_inbound_links() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("tables")).unwrap();
    std::fs::write(
        root.join("tables/orders.md"),
        "---\ntype: Table\n---\n# Overview\n\norders\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("reports")).unwrap();
    std::fs::write(
        root.join("reports/daily.md"),
        "---\ntype: Report\n---\n# Sources\n\n- [Orders](/tables/orders.md)\n",
    )
    .unwrap();

    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "move",
            "tables/orders",
            "facts/orders",
        ],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        !root.join("tables/orders.md").exists(),
        "old file must be gone"
    );
    assert!(root.join("facts/orders.md").exists(), "new file must exist");
    let daily = std::fs::read_to_string(root.join("reports/daily.md")).unwrap();
    assert!(
        daily.contains("- [Orders](/facts/orders.md)\n"),
        "inbound link must be rewritten: {daily}"
    );
}

#[test]
fn mv_existing_destination_exits_two() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.md"), "---\ntype: T\n---\na\n").unwrap();
    std::fs::write(root.join("b.md"), "---\ntype: T\n---\nb\n").unwrap();
    let code = run(&["-C", root.to_str().unwrap(), "move", "a", "b"], None);
    assert_eq!(
        code, 2,
        "mv must refuse to overwrite an existing destination"
    );
}

#[test]
fn mv_missing_source_exits_not_found() {
    let dir = TempDir::new().unwrap();
    let code = run(
        &[
            "-C",
            dir.path().to_str().unwrap(),
            "move",
            "ghost",
            "elsewhere",
        ],
        None,
    );
    assert_eq!(code, 3, "moving a nonexistent concept is not-found");
}

#[test]
fn rm_deletes_concept_and_scrubs_inbound_links() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("tables")).unwrap();
    std::fs::write(
        root.join("tables/orders.md"),
        "---\ntype: Table\n---\n# Overview\n\norders\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("reports")).unwrap();
    std::fs::write(
        root.join("reports/daily.md"),
        "---\ntype: Report\n---\n# Sources\n\n- [Orders](/tables/orders.md)\n",
    )
    .unwrap();

    let code = run(
        &["-C", root.to_str().unwrap(), "remove", "tables/orders"],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        !root.join("tables/orders.md").exists(),
        "deleted file must be gone"
    );
    let daily = std::fs::read_to_string(root.join("reports/daily.md")).unwrap();
    assert!(
        !daily.contains("/tables/orders.md"),
        "inbound bullet link must be scrubbed: {daily}"
    );
}

#[test]
fn rm_missing_concept_exits_not_found() {
    let dir = TempDir::new().unwrap();
    let code = run(
        &["-C", dir.path().to_str().unwrap(), "remove", "ghost"],
        None,
    );
    assert_eq!(code, 3, "deleting a nonexistent concept is not-found");
}

#[test]
fn rm_force_skips_missing_concept() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.md"), "---\ntype: T\n---\na\n").unwrap();
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "remove",
            "--force",
            "a",
            "ghost",
        ],
        None,
    );
    assert_eq!(code, 0, "--force ignores missing ids");
    assert!(
        !root.join("a.md").exists(),
        "existing concept still deleted"
    );
}

#[test]
fn set_reserved_target_is_rejected() {
    let dir = TempDir::new().unwrap();
    let code = run(
        &[
            "-C",
            dir.path().to_str().unwrap(),
            "set",
            "tables/index",
            "--type",
            "Table",
        ],
        None,
    );
    assert_eq!(code, 2, "writing a reserved filename is an input error");
}

#[test]
fn set_auto_regenerates_indexes_by_default() {
    // Through the binary, reindexing is on by default: creating a concept lays
    // down its directory's index.md and the root index pointing at it.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "set",
            "tables/orders",
            "--type",
            "Table",
            "--title",
            "Orders",
        ],
        None,
    );
    assert_eq!(code, 0);
    let tables = std::fs::read_to_string(root.join("tables/index.md")).unwrap();
    assert_eq!(tables, "# Table\n\n* [Orders](orders.md)\n");
    assert!(root.join("index.md").exists(), "root index regenerated");
}

#[test]
fn no_reindex_leaves_indexes_alone() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "set",
            "tables/orders",
            "--type",
            "Table",
            "--no-reindex",
        ],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        !root.join("tables/index.md").exists(),
        "--no-reindex writes no index"
    );
}

#[test]
fn index_regenerate_then_check_is_clean() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    run(
        &[
            "-C",
            root.to_str().unwrap(),
            "set",
            "tables/orders",
            "--type",
            "Table",
            "--no-reindex",
        ],
        None,
    );
    // A check before regeneration sees drift (the missing index), exit 5.
    let drift = run(&["-C", root.to_str().unwrap(), "index", "--check"], None);
    assert_eq!(drift, 5, "missing index is drift");

    let regen = run(
        &["-C", root.to_str().unwrap(), "index", "--regenerate"],
        None,
    );
    assert_eq!(regen, 0);

    // After regeneration the check is clean.
    let clean = run(&["-C", root.to_str().unwrap(), "index", "--check"], None);
    assert_eq!(clean, 0, "regenerated indexes are up to date");
}

#[test]
fn index_check_on_reference_bundle_reports_no_drift() {
    // The committed reference bundle's index.md files are exactly what the
    // generator produces — a regression guard on the index format. --check is
    // read-only, so it is safe to run against the fixture in place.
    let ga4 = fixture("reference/ga4");
    let code = run(&["-C", ga4.to_str().unwrap(), "index", "--check"], None);
    assert_eq!(code, 0, "reference ga4 indexes must match the generator");
}

#[test]
fn check_reports_broken_links_with_issue_exit_code() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // A concept whose only link points at a concept that does not exist.
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "set",
            "tables/orders",
            "--type",
            "Table",
            "--body",
            "See [customers](/tables/customers.md).",
            "--no-reindex",
        ],
        None,
    );
    assert_eq!(code, 0);

    // The dangling link is a problem: check exits 5.
    let dangling = run(&["-C", root.to_str().unwrap(), "check"], None);
    assert_eq!(dangling, 5, "a broken link is a check problem");

    // Create the target; the bundle is now clean.
    let code = run(
        &[
            "-C",
            root.to_str().unwrap(),
            "set",
            "tables/customers",
            "--type",
            "Table",
            "--no-reindex",
        ],
        None,
    );
    assert_eq!(code, 0);
    let clean = run(&["-C", root.to_str().unwrap(), "check"], None);
    assert_eq!(clean, 0, "no broken links once the target exists");
}

#[test]
fn check_on_reference_bundle_is_clean() {
    // The committed reference bundle must have no broken links or unparseable
    // files — a regression guard that `check` agrees with a known-good bundle.
    let ga4 = fixture("reference/ga4");
    let code = run(&["-C", ga4.to_str().unwrap(), "check"], None);
    assert_eq!(code, 0, "reference ga4 bundle must pass check");
}

#[test]
fn skills_install_writes_a_valid_skill_without_a_bundle() {
    // `skills install` derives its content from the CLI model, so it needs no
    // bundle: a bare `--dir` install into a temp tree must succeed and produce a
    // spec-shaped SKILL.md under the required `okf/` directory.
    let dir = TempDir::new().unwrap();
    let code = run(
        &["skills", "install", "--dir", dir.path().to_str().unwrap()],
        None,
    );
    assert_eq!(code, 0);
    let skill = std::fs::read_to_string(dir.path().join("okf/SKILL.md")).unwrap();
    assert!(skill.starts_with("---\nname: okf\n"));
    assert!(skill.contains("### `okf skills install`"));
}

#[test]
fn skills_install_refuses_to_clobber_without_force() {
    let dir = TempDir::new().unwrap();
    let args = ["skills", "install", "--dir", dir.path().to_str().unwrap()];
    assert_eq!(run(&args, None), 0);
    // Second install hits the existing file: exit code 2 (bad usage class).
    assert_eq!(run(&args, None), 2);
    // With --force it overwrites and succeeds again.
    let forced = [
        "skills",
        "install",
        "--dir",
        dir.path().to_str().unwrap(),
        "--force",
    ];
    assert_eq!(run(&forced, None), 0);
}
