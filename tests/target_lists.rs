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
//! `cargo test` on a packaged or vendored copy, as `just package-test` would
//! show.

use std::collections::{BTreeMap, BTreeSet};
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

/// The dependency list of every recipe in the justfile, keyed by recipe name.
///
/// A recipe header is a line that starts in column 0, names the recipe, then
/// `:`, then the recipes it depends on. `:=` is an assignment, not a header.
fn recipe_dependencies(justfile: &str) -> BTreeMap<&str, Vec<&str>> {
    let mut graph = BTreeMap::new();
    for line in justfile.lines() {
        let code = code_before(line, "#");
        if code.starts_with([' ', '\t']) || code.contains(":=") {
            continue;
        }
        let Some((name, dependencies)) = code.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        graph.insert(name, dependencies.split_whitespace().collect());
    }
    graph
}

/// Every recipe `just entry` ends up running, `entry` included.
fn reachable_from<'a>(
    entry: &'a str,
    graph: &BTreeMap<&'a str, Vec<&'a str>>,
) -> BTreeSet<&'a str> {
    let mut reached = BTreeSet::new();
    let mut pending = vec![entry];
    while let Some(recipe) = pending.pop() {
        if !reached.insert(recipe) {
            continue;
        }
        pending.extend(graph.get(recipe).into_iter().flatten());
    }
    reached
}

/// The recipes a type-check tier depends on, and that nothing else replaces.
///
/// A recipe here is one whose whole purpose is to compile a configuration no
/// other recipe compiles, so dropping it from the gate costs the coverage
/// silently — nothing goes red, the configuration simply stops being built.
const RECIPES_THE_GATE_MUST_REACH: &[&str] =
    &["check-cross", "check-non-unix", "check-no-default-features"];

/// The recipes above are only worth guarding if the gate still runs them.
///
/// `every_target_list_carries_the_whole_table` and
/// `the_best_effort_tier_is_type_checked` both assert what `check-cross` and
/// `check-non-unix` *contain*. Neither asks whether anything runs them, so
/// deleting both from `check` leaves every guard here green while CI silently
/// stops type-checking Android, FreeBSD, NetBSD and illumos — the failure this
/// file's header blames for the Android break reaching a release.
///
/// Reachability, not one hop: an earlier version of this test asked only
/// whether `check` still listed them, and dropping `check` from `ci` one link
/// further up severed the same tier with the guard still green. Asking what
/// `ci` reaches covers every link at once, however the recipes are rearranged.
#[test]
fn the_cross_recipes_are_reachable_from_the_gate() {
    let justfile = read("justfile");
    let graph = recipe_dependencies(&justfile);
    let reached = reachable_from("ci", &graph);

    assert!(
        reached.contains("test"),
        "`ci` reaches neither the test recipe nor, presumably, anything else: \
         {reached:?}. The justfile's shape probably outgrew the parser above"
    );
    for recipe in RECIPES_THE_GATE_MUST_REACH {
        assert!(
            reached.contains(*recipe),
            "`just ci` no longer reaches `{recipe}`, so nothing runs it: the \
             configuration it exists to compile silently stops being built, \
             and the guards over its contents prove nothing. `ci` reaches: \
             {reached:?}"
        );
    }
}

/// And the gate is only worth guarding if CI still calls it.
///
/// `the_cross_recipes_are_reachable_from_the_gate` starts at `ci` because that
/// is what the fast tier runs. Pointing that tier at a narrower recipe drops
/// the whole static-check half of CI — format, lint, deny, doc, MSRV and both
/// cross checks — and leaves the justfile, and so every guard reading it,
/// untouched.
#[test]
fn the_fast_tier_still_runs_the_gate() {
    let workflow = read(".github/workflows/ci.yml");
    let runs_the_gate = workflow.lines().any(|line| {
        let command = line.trim().strip_prefix("run:").unwrap_or(line);
        command.split_whitespace().eq(["just", "ci"])
    });
    assert!(
        runs_the_gate,
        "no step in .github/workflows/ci.yml runs `just ci`, so the recipes \
         the gate depends on are never run in CI, whatever the justfile says"
    );
}
