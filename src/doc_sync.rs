//! Guards that keep the shipped docs from drifting out of sync with source.
//!
//! The README and the crate front page (`lib.rs`) are two hand-maintained
//! entry docs with overlapping facts; those facts have drifted before. These
//! tests pin the enumerable ones — MSRV, the exit-code table, the platform
//! list — against their single source of truth. A failure means a doc went
//! stale: fix the doc (or the code, if the code is what moved).

use crate::DaemonizeError;
use std::collections::{BTreeMap, BTreeSet};

/// The text of a repository file these guards read, baked in at compile time.
///
/// This module ships with the crate, so `cargo test` on a packaged or vendored
/// copy runs it against only the files `Cargo.toml`'s `include` list carries.
/// A guard that reads the repository at run time passes here and panics for
/// that consumer, which is what shipping a guard that read the justfile did.
///
/// `include_str!` moves the failure from the consumer's run to a compile
/// error naming the missing file. A guard that needs an unpublished file
/// belongs in `tests/`, which is not packaged.
fn read(rel: &str) -> &'static str {
    match rel {
        "README.md" => include_str!("../README.md"),
        "src/lib.rs" => include_str!("lib.rs"),
        "examples/echo_server.rs" => include_str!("../examples/echo_server.rs"),
        other => panic!(
            "{other} is not one of the files doc_sync bakes in; add an \
             include_str! arm for it (and the path to Cargo.toml's include \
             list), or move the guard to tests/, which is not packaged"
        ),
    }
}

/// The front page shows consumers how to gate the `daemonize` call for an
/// exotic target. Consumers cannot see this crate's capability aliases, so that
/// example spells out `target_os` — a copy of the table that would otherwise
/// go stale the next time a platform is added.
///
/// The example carries the list twice, once for the checked call and once for
/// the `not(...)` fallback, and both have to move together: a platform added to
/// only the positive list leaves an example in which both branches are live on
/// that target. So each branch is compared with the table on its own.
///
/// Per branch, not per file: counting `target_os` occurrences over the whole
/// file says "the file mentions each name twice", which is a different claim.
/// It goes red on the first genuine platform `cfg` the file grows, and it
/// passes vacuously when a name dropped from one branch is restored to the
/// count by an unrelated `cfg` elsewhere — the very drift this exists to catch.
#[test]
fn front_page_cfg_example_lists_every_target_os() {
    // Both files spell the list out for consumers, who cannot see this crate's
    // aliases: the front page teaches the pattern and the example runs it.
    // Each is a copy of the table, so each is pinned to it.
    let table: BTreeSet<&str> = env!("BLIVET_SUPPORTED_TARGET_OS").split(',').collect();
    for doc in ["src/lib.rs", "examples/echo_server.rs"] {
        let text = read(doc);
        // The fallback opener is searched for first and is the longer literal,
        // so the checked branch's opener cannot match inside it.
        for opener in ["#[cfg(not(any(", "#[cfg(any("] {
            let branch = cfg_branch(text, opener, doc);
            assert_eq!(
                target_os_clauses(branch)
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                table,
                "{doc}'s `{opener}` branch must name exactly the platforms the \
                 capability table does — a name missing here leaves the example \
                 with both branches live on that target, and a name the table \
                 lacks sends a reader to the `#[deprecated]` stub that panics"
            );
        }
    }
}

/// The text of the `cfg` clause list that `opener` opens.
fn cfg_branch<'a>(text: &'a str, opener: &str, doc: &str) -> &'a str {
    let at = text
        .find(opener)
        .unwrap_or_else(|| panic!("{doc}'s `cfg` example has no `{opener}` branch"));
    let rest = &text[at + opener.len()..];
    let end = rest
        .find("))")
        .unwrap_or_else(|| panic!("{doc}'s `{opener}` branch is never closed"));
    &rest[..end]
}

/// Every distinct `target_os` a `cfg` clause in `text` names.
fn target_os_clauses(text: &str) -> Vec<&str> {
    const CLAUSE: &str = "target_os = \"";
    let mut names = Vec::new();
    for (at, _) in text.match_indices(CLAUSE) {
        let rest = &text[at + CLAUSE.len()..];
        if let Some(end) = rest.find('"') {
            let name = &rest[..end];
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// Every `DaemonizeError` variant's documented exit code (the README "Errors &
/// exit codes" table) must equal `exit_code()`, and the table must list every
/// variant — no more, no less.
#[test]
fn readme_exit_codes_match_error_impl() {
    // One constructed sample per variant. The exhaustive match in
    // `variant_is_covered` below is the compile-time ratchet: adding a variant
    // breaks the build until it is listed here *and* in the README table.
    let dummy_io = || std::io::Error::from(std::io::ErrorKind::Other);
    let samples: Vec<(&str, DaemonizeError)> = vec![
        (
            "ValidationError",
            DaemonizeError::ValidationError("x".into()),
        ),
        (
            "ProgramNotFound",
            DaemonizeError::ProgramNotFound("x".into()),
        ),
        ("UserNotFound", DaemonizeError::UserNotFound("x".into())),
        ("GroupNotFound", DaemonizeError::GroupNotFound("x".into())),
        (
            "LockConflict",
            DaemonizeError::LockConflict { path: "/x".into() },
        ),
        ("LockfileError", DaemonizeError::LockfileError("x".into())),
        ("PidfileError", DaemonizeError::PidfileError("x".into())),
        (
            "OutputFileError",
            DaemonizeError::OutputFileError("x".into()),
        ),
        ("ChownError", DaemonizeError::ChownError("x".into())),
        ("ForkFailed", DaemonizeError::ForkFailed("x".into())),
        ("SetsidFailed", DaemonizeError::SetsidFailed("x".into())),
        ("ChdirFailed", DaemonizeError::ChdirFailed("x".into())),
        ("SystemError", DaemonizeError::SystemError("x".into())),
        (
            "PermissionDenied",
            DaemonizeError::PermissionDenied("x".into()),
        ),
        ("ExecFailed", DaemonizeError::ExecFailed("x".into())),
        ("NotifyFailed", DaemonizeError::NotifyFailed(dummy_io())),
        ("PrivilegesNotDropped", DaemonizeError::PrivilegesNotDropped),
        ("Application", DaemonizeError::application(42, "x")),
    ];
    for (_, err) in &samples {
        variant_is_covered(err);
    }

    let documented = parse_exit_code_table();
    let sample_names: std::collections::BTreeSet<&str> = samples.iter().map(|(n, _)| *n).collect();
    let table_names: std::collections::BTreeSet<&str> =
        documented.keys().map(String::as_str).collect();
    assert_eq!(
        sample_names, table_names,
        "README exit-code table and DaemonizeError variants must match exactly"
    );

    for (name, err) in &samples {
        let cell = &documented[*name];
        if *name == "Application" {
            assert_eq!(cell, "caller's", "Application row should read \"caller's\"");
            assert_eq!(
                err.exit_code(),
                42,
                "Application forwards the caller's code"
            );
        } else {
            let code: u8 = cell
                .parse()
                .unwrap_or_else(|_| panic!("{name} exit code cell '{cell}' is not a number"));
            assert_eq!(
                code,
                err.exit_code(),
                "{name} exit code drifted from the README"
            );
        }
    }
}

/// Compile-time ratchet: a new `DaemonizeError` variant fails to compile here,
/// forcing its addition to the sample list and the README table above.
fn variant_is_covered(err: &DaemonizeError) {
    match err {
        DaemonizeError::ValidationError(_)
        | DaemonizeError::ProgramNotFound(_)
        | DaemonizeError::UserNotFound(_)
        | DaemonizeError::GroupNotFound(_)
        | DaemonizeError::LockConflict { .. }
        | DaemonizeError::LockfileError(_)
        | DaemonizeError::PidfileError(_)
        | DaemonizeError::OutputFileError(_)
        | DaemonizeError::ChownError(_)
        | DaemonizeError::ForkFailed(_)
        | DaemonizeError::SetsidFailed(_)
        | DaemonizeError::ChdirFailed(_)
        | DaemonizeError::SystemError(_)
        | DaemonizeError::PermissionDenied(_)
        | DaemonizeError::ExecFailed(_)
        | DaemonizeError::NotifyFailed(_)
        | DaemonizeError::PrivilegesNotDropped
        | DaemonizeError::Application { .. } => {}
    }
}

/// Parse the README "Errors & exit codes" table into `variant -> code cell`.
fn parse_exit_code_table() -> BTreeMap<String, String> {
    let readme = read("README.md");
    let section = readme
        .split("### Errors & exit codes")
        .nth(1)
        .expect("README has an 'Errors & exit codes' section")
        .split("\n## ")
        .next()
        .unwrap();

    let mut map = BTreeMap::new();
    for line in section.lines() {
        let line = line.trim();
        // Data rows start with a backticked variant name; the header and the
        // `| --- |` separator do not.
        if !line.starts_with("| `") {
            continue;
        }
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let variant = cells[1].trim_matches('`').to_string();
        map.insert(variant, cells[2].to_string());
    }
    assert!(!map.is_empty(), "parsed no rows from the exit-code table");
    map
}
