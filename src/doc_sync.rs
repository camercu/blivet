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
        "Cargo.toml" => include_str!("../Cargo.toml"),
        "README.md" => include_str!("../README.md"),
        "docs/SPEC.md" => include_str!("../docs/SPEC.md"),
        "src/lib.rs" => include_str!("lib.rs"),
        "src/context.rs" => include_str!("context.rs"),
        "examples/echo_server.rs" => include_str!("../examples/echo_server.rs"),
        other => panic!(
            "{other} is not one of the files doc_sync bakes in; add an \
             include_str! arm for it (and the path to Cargo.toml's include \
             list), or move the guard to tests/, which is not packaged"
        ),
    }
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

/// Every shipped file that spells the platform list out must name every
/// supported platform.
///
/// The list comes from the capability table in `build.rs`, the same table the
/// `cfg` aliases come from, so adding a platform there is what makes this test
/// demand the docs mention it.
///
/// `src/context.rs` is here because it was the one that went stale: Android
/// reached the capability table, the README and the front page, and
/// `drop_privileges`' own rustdoc kept telling docs.rs readers the method was a
/// panicking stub there. ADR 0001 promises a platform is one table row rather
/// than "4 prose edits with no backstop"; a copy of the list that no guard
/// reads is one of those prose edits. A shipped file that grows a copy of the
/// list belongs in [`FILES_LISTING_PLATFORMS`], and needs an `include_str!` arm
/// in [`read`].
///
/// This is the prose-level check, and a deliberately loose one: it asks only
/// whether the name appears somewhere in the file, so an unrelated mention
/// satisfies it. The claims that matter are held tighter elsewhere — the tier
/// tables by [`supported_tier_matches_the_capability_table`], which compares
/// them as sets in both directions, and the front page's `cfg` example by
/// [`front_page_cfg_example_lists_every_target_os`]. Read a pass here as "the
/// platform is mentioned", not as "the platform is documented correctly".
#[test]
fn platform_list_consistent() {
    for file in FILES_LISTING_PLATFORMS {
        let text = read(file);
        for platform in env!("BLIVET_PLATFORMS").split(',') {
            assert!(
                text.contains(platform),
                "{file} omits supported platform {platform}"
            );
        }
    }
}

/// The dependency names in one `Cargo.toml` table, normalized.
///
/// `serial_test` and `signal-hook` are the same kind of name written two ways,
/// so both spellings fold to one before any comparison.
#[cfg(test)]
fn declared_dependencies(table: &str) -> std::collections::BTreeSet<String> {
    let manifest = read("Cargo.toml");
    let section = manifest
        .split_once(&format!("[{table}]\n"))
        .unwrap_or_else(|| panic!("Cargo.toml has a [{table}] table"))
        .1;
    let section = section.split("\n[").next().expect("a table body");
    section
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, _)| name.trim().replace('-', "_"))
        .filter(|name| !name.is_empty())
        .collect()
}

/// The SPEC's dependency paragraph must name the dependencies the crate has.
///
/// SPEC lists them in prose — "Dependencies: nix (…), clap, thiserror, libc.
/// Dev: tempfile, …" — which is a copy of two `Cargo.toml` tables that nothing
/// compared with them. It drifted three ways at once: `signal-hook` and
/// `mutants` were added and never listed, and `clap` became optional while the
/// prose still read as though every consumer compiles it.
///
/// This is the same shape as [`platform_list_consistent`] — a set the manifest
/// already states, copied into prose — and gets the same treatment: the copy is
/// allowed to exist, but only under a guard.
///
/// Set equality in both directions. A missing name is the drift that happened;
/// an extra name is a dependency dropped from the manifest and left in the
/// prose, which reads as a supply-chain claim the crate no longer makes.
#[test]
fn spec_dependency_list_matches_cargo_toml() {
    let spec = read("docs/SPEC.md");
    let paragraph = spec
        .split_once("\nDependencies:")
        .expect("SPEC has a `Dependencies:` paragraph")
        .1
        .split_once("\n\n")
        .expect("the paragraph ends")
        .0;

    for table in ["dependencies", "dev-dependencies"] {
        for name in declared_dependencies(table) {
            assert!(
                paragraph.replace('-', "_").contains(&name),
                "SPEC's dependency paragraph omits `{name}`, a [{table}] entry"
            );
        }
    }

    let declared: std::collections::BTreeSet<String> = ["dependencies", "dev-dependencies"]
        .iter()
        .flat_map(|table| declared_dependencies(table))
        .collect();
    for word in paragraph.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
        let name = word.trim().replace('-', "_");
        if name.len() > 2 && declared.iter().any(|d| d.eq_ignore_ascii_case(&name)) {
            continue;
        }
        assert!(
            !KNOWN_CRATE_NAMES.contains(&name.as_str()),
            "SPEC's dependency paragraph names `{word}`, which is not in \
             Cargo.toml any more"
        );
    }
}

/// Crate names the SPEC's dependency paragraph has named at some point.
///
/// The reverse direction of [`spec_dependency_list_matches_cargo_toml`] cannot
/// simply treat every word in the paragraph as a crate name — the prose around
/// the list is words too. This is the small closed set it checks against, so
/// dropping a dependency from `Cargo.toml` and leaving it in the prose fails.
#[cfg(test)]
const KNOWN_CRATE_NAMES: &[&str] = &[
    "clap",
    "libc",
    "mutants",
    "nix",
    "relentless",
    "serial_test",
    "signal_hook",
    "tempfile",
    "thiserror",
];

/// The shipped files that spell the platform list out in prose.
///
/// See [`platform_list_consistent`], which is what holds them to the capability
/// table.
#[cfg(test)]
const FILES_LISTING_PLATFORMS: &[&str] = &["README.md", "src/lib.rs", "src/context.rs"];

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

/// Platforms named in the Supported row's `Platforms` column.
///
/// The column is found by its header, in the nearest header row above the
/// Supported row, rather than by position. Reading the last cell survives a
/// column added on the left and breaks on one added on the right — a Notes or
/// Since column, the likelier addition — and the break would read as
/// `build.rs` disagreeing with a doc that in fact still agrees.
fn supported_tier_row(doc: &str) -> Vec<String> {
    let rows: Vec<Vec<&str>> = doc
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('|'))
        .map(|l| l.split('|').map(str::trim).collect())
        .collect();

    let supported = rows
        .iter()
        .position(|cells| {
            cells
                .get(1)
                .is_some_and(|c| c.trim_matches('*') == "Supported")
        })
        .expect("the tier table has a Supported row");

    let column = rows[..supported]
        .iter()
        .rev()
        .find_map(|cells| cells.iter().position(|c| *c == "Platforms"))
        .expect("the tier table has a Platforms column above its Supported row");

    rows[supported]
        .get(column)
        .unwrap_or_else(|| {
            panic!(
                "the Supported row has no cell under the Platforms column, got: {:?}",
                rows[supported]
            )
        })
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
