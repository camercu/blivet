//! Guards over the target lists that live outside the capability table.
//!
//! ADR 0001 promises that adding a platform is one table row. It was three:
//! the docs.rs target list in `Cargo.toml` and the `check-cross` recipe in the
//! justfile are hand-maintained copies, and nothing noticed when they
//! disagreed with the table. The second is the gap ADR 0002 blames for the
//! Android build break reaching a release — "no gate ever asked whether
//! Android compiled".
//!
//! These live in `tests/`, not beside the other doc guards in
//! `src/doc_sync.rs`, because they read the justfile: `src/` ships with the
//! crate and the justfile does not, so a guard there panics for anyone running
//! `cargo test` on a packaged or vendored copy. `tests/packaging.rs` holds
//! that rule for the whole shipped tree.

use std::path::PathBuf;

mod common;
use common::code_before;

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The rustc triple token that identifies each supported `target_os`.
///
/// A fact about rustc's naming, not a support decision — macOS's triple says
/// `apple-darwin` and Android's says `linux-android`, so a substring test on
/// the `target_os` alone would miss one and confuse the other with Linux.
fn triple_token(target_os: &str) -> String {
    match target_os {
        "macos" => "apple-darwin".to_string(),
        "android" => "linux-android".to_string(),
        "linux" => "unknown-linux".to_string(),
        other => format!("unknown-{other}"),
    }
}

/// The triples in `Cargo.toml`'s `[package.metadata.docs.rs]` targets array.
fn docs_rs_targets(cargo_toml: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut inside = false;
    for line in cargo_toml.lines() {
        let code = code_before(line, "#").trim();
        if code.starts_with("targets") && code.contains('[') {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if code.starts_with(']') {
            break;
        }
        let entry = code.trim_end_matches(',').trim_matches('"');
        if !entry.is_empty() {
            targets.push(entry.to_string());
        }
    }
    assert!(!targets.is_empty(), "parsed no docs.rs targets");
    targets
}

/// The triples the justfile actually runs a `cargo check` for.
///
/// Read from the `--target` of a line that also runs a check, not from
/// anywhere the triple is merely written: `check-cross` opens with a
/// `rustup target add` naming every triple, so a guard that asked only whether
/// the name appeared was satisfied by that one line and would have watched
/// every check below it be deleted.
fn checked_targets(justfile: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for line in justfile.lines() {
        let code = code_before(line, "#");
        if !code.contains("check") {
            continue;
        }
        let mut words = code.split_whitespace();
        while let Some(word) = words.next() {
            if word == "--target" {
                if let Some(triple) = words.next() {
                    targets.push(triple.to_string());
                }
            }
        }
    }
    assert!(!targets.is_empty(), "parsed no checked targets");
    targets
}

#[test]
fn every_target_list_carries_the_whole_table() {
    let cargo_toml = read("Cargo.toml");
    let justfile = read("justfile");
    let docs_rs = docs_rs_targets(&cargo_toml);
    let checked = checked_targets(&justfile);

    // (what carries the list, the triples in it, [(target_os, why absent)]).
    let consumers: [(&str, &[String], &[(&str, &str)]); 2] = [
        (
            "Cargo.toml's docs.rs target list",
            &docs_rs,
            // OpenBSD is tier 3 and has no prebuilt std for docs.rs to build
            // against; Linux is the default-target rather than a listed one.
            &[
                ("openbsd", "no prebuilt std"),
                ("linux", "the default-target"),
            ],
        ),
        (
            "the check-cross recipe",
            &checked,
            // OpenBSD has no rustup std to check against, and the CI matrix
            // builds on a macOS host, which is a stronger check than a
            // cross-check would be.
            &[("openbsd", "no rustup std"), ("macos", "a CI host")],
        ),
    ];

    for (what, triples, exempt) in consumers {
        for target_os in env!("BLIVET_SUPPORTED_TARGET_OS").split(',') {
            if let Some((_, why)) = exempt.iter().find(|(os, _)| *os == target_os) {
                assert!(
                    !why.is_empty(),
                    "an exemption must say why {target_os} is absent"
                );
                continue;
            }
            let token = triple_token(target_os);
            assert!(
                triples.iter().any(|t| t.contains(&token)),
                "{what} omits {target_os} ({token}), which the capability \
                 table supports; it carries {triples:?}"
            );
        }
    }
}

#[test]
fn the_best_effort_tier_is_type_checked() {
    // The Best-effort row promises a Unix the table does not list compiles.
    // illumos stands in for the class, and this check is the row's only
    // backing — so it is the one check whose deletion the guard above cannot
    // notice, since no table row demands it.
    let justfile = read("justfile");
    let line = justfile
        .lines()
        .map(|l| code_before(l, "#"))
        .find(|code| code.contains("--target x86_64-unknown-illumos"))
        .expect("a recipe must type-check illumos: it backs the Best-effort tier");
    assert!(
        line.contains("--all-targets"),
        "the illumos check must cover --all-targets: what breaks on a \
         best-effort target is the shipped example and the test helpers \
         reaching for a capability-gated item, not the library: {line}"
    );
    assert!(
        line.contains("-D warnings"),
        "the illumos check must run under -D warnings: a best-effort target \
         resolves to the #[deprecated] stubs, and a deprecation is a warning: \
         {line}"
    );
}

/// The recipes above are only worth guarding if the gate still runs them.
///
/// `every_target_list_carries_the_whole_table` and
/// `the_best_effort_tier_is_type_checked` both assert what `check-cross` and
/// `check-non-unix` *contain*. Neither asks whether `check` still depends on
/// them, so deleting both from the dependency list leaves every guard here
/// green while CI silently stops type-checking Android, FreeBSD, NetBSD and
/// illumos — the failure this file's header blames for the Android break
/// reaching a release.
#[test]
fn the_gate_still_depends_on_the_cross_recipes() {
    let justfile = read("justfile");
    let line = justfile
        .lines()
        .map(|l| code_before(l, "#"))
        .find(|l| l.trim_start().starts_with("check:"))
        .expect("the justfile has a `check:` recipe");
    let dependencies: Vec<&str> = line.split_whitespace().skip(1).collect();

    for recipe in ["check-cross", "check-non-unix"] {
        assert!(
            dependencies.contains(&recipe),
            "`check` no longer depends on `{recipe}`, so nothing runs it and \
             the guards over its contents prove nothing. Dependencies are: \
             {dependencies:?}"
        );
    }
}
