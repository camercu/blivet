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
//! `cargo test` on a packaged or vendored copy.

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

#[test]
fn every_target_list_carries_the_whole_table() {
    let cargo_toml = read("Cargo.toml");
    let justfile = read("justfile");
    let docs_rs = docs_rs_targets(&cargo_toml);
    let justfile_words: Vec<String> = justfile.split_whitespace().map(str::to_string).collect();

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
            &justfile_words,
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

/// The list of files inside `Cargo.toml`'s `include` array.
fn included_paths(cargo_toml: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut inside = false;
    for line in cargo_toml.lines() {
        let code = code_before(line, "#").trim();
        if code.starts_with("include") && code.contains('[') {
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
            paths.push(entry.trim_start_matches('/').to_string());
        }
    }
    assert!(!paths.is_empty(), "parsed no include entries");
    paths
}

/// True where `include` carries `path`.
fn is_packaged(include: &[String], path: &str) -> bool {
    // Cargo adds these whatever the allowlist says.
    if path == "Cargo.toml" || path == "Cargo.lock" {
        return true;
    }
    include
        .iter()
        .any(|entry| match entry.strip_suffix("/**/*.rs") {
            Some(dir) => path.starts_with(&format!("{dir}/")) && path.ends_with(".rs"),
            None => entry == path,
        })
}

#[test]
fn every_file_the_shipped_guards_read_is_packaged() {
    // `src/doc_sync.rs` ships with the crate, so `cargo test` on a packaged or
    // vendored copy runs it with only the `include` list present. A guard
    // there that reads anything else passes here and panics for a consumer —
    // which is what shipping a guard that read the justfile did. `read`
    // refuses a path outside INPUTS; this checks INPUTS against `include`, so
    // the two halves cannot drift.
    let cargo_toml = read("Cargo.toml");
    let include = included_paths(&cargo_toml);
    let doc_sync = read("src/doc_sync.rs");

    let start = doc_sync
        .find("const INPUTS")
        .expect("src/doc_sync.rs declares the files its guards read as INPUTS");
    let end = doc_sync[start..]
        .find("];")
        .expect("INPUTS is an array literal")
        + start;
    let inputs: Vec<&str> = doc_sync[start..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|s| !s.is_empty())
        .collect();
    assert!(!inputs.is_empty(), "parsed no INPUTS entries");

    for input in inputs {
        assert!(
            is_packaged(&include, input),
            "src/doc_sync.rs reads {input}, which Cargo.toml's include list \
             does not carry: a packaged copy would panic. Add it to include, \
             or move the guard to tests/, which is not packaged."
        );
    }
}
