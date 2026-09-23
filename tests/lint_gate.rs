//! The clippy gate must see the test sources, not just the shipped crate.
//!
//! `cargo clippy` without `--all-targets` compiles the library and the binary
//! and stops. No test binary, no `#[cfg(test)]` module — so every lint in
//! `tests/` and in the crate's own unit tests goes unread and the gate still
//! exits 0. That is not hypothetical: the flag was missing, and a
//! `clippy::type_complexity` violation sat in `tests/target_lists.rs` behind
//! it, green, until someone ran clippy by hand.
//!
//! The rule is checked here rather than written down because the failure is
//! silent — a narrowed gate looks exactly like a passing one.

use std::path::PathBuf;

fn justfile() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("justfile");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The body of a `just` recipe: its lines are the ones indented under it.
fn recipe_body(justfile: &str, name: &str) -> String {
    let header = format!("{name}:");
    let mut lines = justfile.lines().skip_while(|l| !l.starts_with(&header));
    assert!(
        lines.next().is_some(),
        "the justfile has no `{name}:` recipe; if it was renamed, this guard \
         names the wrong one and is no longer checking anything"
    );
    lines
        .take_while(|l| l.starts_with(char::is_whitespace) || l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_clippy_gate_covers_every_target() {
    let body = recipe_body(&justfile(), "lint");
    assert!(
        body.contains("clippy"),
        "the `lint` recipe no longer runs clippy, so this guard is checking \
         nothing: {body}"
    );
    assert!(
        body.contains("--all-targets"),
        "the `lint` recipe runs clippy without --all-targets, so no test \
         binary and no #[cfg(test)] module is linted and the gate passes \
         having read none of them: {body}"
    );
}

#[test]
fn the_clippy_gate_still_denies_warnings() {
    let body = recipe_body(&justfile(), "lint");
    assert!(
        body.contains("-D warnings"),
        "the `lint` recipe stopped denying warnings, so clippy reports and \
         exits 0: {body}"
    );
}
