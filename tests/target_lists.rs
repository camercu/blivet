//! Guards over the target lists that live outside the capability table.
//!
//! ADR 0001 promises that adding a platform is one table row. It was three:
//! the docs.rs target list in `Cargo.toml` and the `check-cross` recipe in the
//! justfile are hand-maintained copies, and nothing noticed when they
//! disagreed with the table. The second is the gap ADR 0002 blames for the
//! Android build break reaching a release — "no gate ever asked whether
//! Android compiled".
//!
//! This is a text guard, the last resort, and it is kept because the other
//! rungs do not reach. `Cargo.toml` is static, so its docs.rs list cannot be
//! generated from the table; no tier runs on docs.rs, so a missing platform
//! shows up only as missing documentation after a release; and a platform
//! missing from `check-cross` fails nothing at all — the gap ADR 0002 blames
//! for Android breaking in a release. So the lists are compared with the table
//! as sets. Which recipes the gate runs is left to review.
//!
//! It lives in `tests/`, not `src/`, because it reads the justfile, which the
//! crate does not ship.

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

/// One list of target triples that must carry the whole capability table:
/// what holds the list, the triples in it, and the `target_os` values allowed
/// to be absent from it with the reason each one is.
type TargetListConsumer<'a> = (&'a str, &'a [String], &'a [(&'a str, &'a str)]);

#[test]
fn every_target_list_carries_the_whole_table() {
    let cargo_toml = read("Cargo.toml");
    let justfile = read("justfile");
    let docs_rs = docs_rs_targets(&cargo_toml);
    let checked = checked_targets(&justfile);

    let consumers: [TargetListConsumer; 2] = [
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
