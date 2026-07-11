//! Guards that keep the shipped docs from drifting out of sync with source.
//!
//! The README and the crate front page (`lib.rs`) are two hand-maintained
//! entry docs with overlapping facts; those facts have drifted before. These
//! tests pin the enumerable ones — MSRV, the exit-code table, the platform
//! list — against their single source of truth. A failure means a doc went
//! stale: fix the doc (or the code, if the code is what moved).

use crate::DaemonizeError;
use std::collections::BTreeMap;
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

/// The README and the crate front page must name the same supported platforms.
#[test]
fn platform_list_consistent() {
    let platforms = ["Linux", "macOS", "FreeBSD", "NetBSD", "OpenBSD"];
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
