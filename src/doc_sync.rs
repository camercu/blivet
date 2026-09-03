//! Guards that keep the shipped docs from drifting out of sync with source.
//!
//! The README and the crate front page (`lib.rs`) are two hand-maintained
//! entry docs with overlapping facts; those facts have drifted before. These
//! tests pin the enumerable ones — MSRV, the exit-code table, the platform
//! list — against their single source of truth. A failure means a doc went
//! stale: fix the doc (or the code, if the code is what moved).

use crate::DaemonizeError;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// `rust-version` in `Cargo.toml` is the one MSRV; the README must echo it in
/// both the badge and the "Minimum supported Rust version" section.
#[test]
fn readme_msrv_matches_cargo_toml() {
    let cargo = read("Cargo.toml");
    let msrv = cargo
        .lines()
        .find_map(|l| l.trim().strip_prefix("rust-version = "))
        .expect("rust-version in Cargo.toml")
        .trim()
        .trim_matches('"');

    let readme = read("README.md");
    assert!(
        readme.contains(&format!("MSRV-{msrv}-")),
        "README MSRV badge should reference {msrv}"
    );
    assert!(
        readme.contains(&format!("\n{msrv}\n")),
        "README 'Minimum supported Rust version' section should state {msrv}"
    );
}

/// The README and the crate front page must name every supported platform.
///
/// The list comes from the capability table in `build.rs`, the same table the
/// `cfg` aliases come from, so adding a platform there is what makes this test
/// demand the docs mention it.
#[test]
fn platform_list_consistent() {
    let platforms = env!("BLIVET_PLATFORMS").split(',');
    let readme = read("README.md");
    let front_page = read("src/lib.rs");
    for p in platforms {
        assert!(readme.contains(p), "README omits supported platform {p}");
        assert!(
            front_page.contains(p),
            "crate front page (lib.rs) omits supported platform {p}"
        );
    }
}

/// The Supported row of the tier table, in `README.md` and `docs/SPEC.md`,
/// must name exactly the platforms the capability table has.
///
/// [`platform_list_consistent`] checks one direction — that every supported
/// platform is mentioned — which leaves the direction that matters more
/// unguarded: a tier table may claim a platform `build.rs` has never heard of,
/// and claiming support the project cannot back is the failure the tiering
/// exists to prevent. Set equality closes both.
#[test]
fn supported_tier_matches_the_capability_table() {
    let expected: BTreeSet<&str> = env!("BLIVET_PLATFORMS").split(',').collect();

    for doc in ["README.md", "docs/SPEC.md"] {
        let listed = supported_tier_row(&read(doc));
        assert_eq!(
            listed.iter().map(String::as_str).collect::<BTreeSet<_>>(),
            expected,
            "{doc}'s Supported tier row and the build.rs capability table must \
             name the same platforms"
        );
    }
}

/// Platforms named in the last cell of the tier table's Supported row.
fn supported_tier_row(doc: &str) -> Vec<String> {
    let row = doc
        .lines()
        .map(str::trim)
        .find(|l| {
            l.starts_with('|')
                && l.split('|')
                    .nth(1)
                    .is_some_and(|c| c.trim().trim_matches('*') == "Supported")
        })
        .expect("the tier table has a Supported row");
    let cells: Vec<&str> = row.split('|').map(str::trim).collect();
    cells[3]
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// The front page shows consumers how to gate the `daemonize` call for an
/// exotic target. Consumers cannot see this crate's capability aliases, so that
/// example spells out `target_os` — a copy of the table that would otherwise
/// go stale the next time a platform is added.
///
/// The example carries the list twice, once for the checked call and once for
/// the `not(...)` fallback, and both have to move together: a platform added to
/// only the positive list leaves an example in which both branches are live on
/// that target. So this counts the clauses rather than asking whether each
/// appears somewhere, which one list alone would satisfy.
#[test]
fn front_page_cfg_example_lists_every_target_os() {
    const BRANCHES: usize = 2;

    let front_page = read("src/lib.rs");
    for os in env!("BLIVET_SUPPORTED_TARGET_OS").split(',') {
        let clause = format!("target_os = \"{os}\"");
        assert_eq!(
            front_page.matches(&clause).count(),
            BRANCHES,
            "the front page `cfg` example must name {clause} in both the \
             checked branch and the `not(...)` fallback"
        );
    }
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
