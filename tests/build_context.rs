//! Guard: the Docker build context excludes everything the working tree does.
//!
//! The container tiers copy the whole repository in (`COPY . .`), so anything
//! `.gitignore` names as local working state — build output, editor state, a
//! review ledger — rides into the image unless `.dockerignore` names it too.
//! The cost is quiet: a bigger context, and an edit to a file that has nothing
//! to do with the build invalidating the `COPY` layer, so the tier recompiles
//! from scratch.
//!
//! The two lists drifted exactly that way once `/.harden/` joined `.gitignore`
//! and not `.dockerignore`. They are hand-maintained copies, which is the same
//! shape `tests/target_lists.rs` guards for the platform lists.
//!
//! `.dockerignore` may name more than `.gitignore` does — `.git/` is the
//! standing example — so this is a subset check in one direction only.

use std::path::PathBuf;

/// An ignore entry with its anchoring and directory markers removed, so
/// `/target`, `target/` and `target` all compare equal.
fn normalized(line: &str) -> Option<String> {
    let entry = line.split('#').next().unwrap_or_default().trim();
    if entry.is_empty() || entry.starts_with('!') {
        return None;
    }
    Some(
        entry
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_owned(),
    )
}

fn entries(rel: &str) -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines().filter_map(normalized).collect()
}

#[test]
fn every_ignored_path_stays_out_of_the_docker_build_context() {
    let ignored = entries(".gitignore");
    assert!(
        !ignored.is_empty(),
        "parsed no entries from .gitignore; if it is empty, so is this test"
    );
    let context = entries(".dockerignore");

    let missing: Vec<&String> = ignored.iter().filter(|e| !context.contains(e)).collect();
    assert!(
        missing.is_empty(),
        ".gitignore names local working state that .dockerignore does not, so \
         `COPY . .` carries it into the tier images and every edit to it \
         invalidates the layer: {missing:?}"
    );
}
