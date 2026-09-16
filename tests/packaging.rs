//! Guards over what ships to crates.io.
//!
//! `src/` ships with the crate and the rest of the repository does not, so a
//! `#[cfg(test)]` guard in `src/` runs on a packaged or vendored copy with
//! only `Cargo.toml`'s `include` list present. One of them read the justfile
//! through `CARGO_MANIFEST_DIR` and panicked for exactly that consumer.
//!
//! `src/doc_sync.rs` bakes its inputs in with `include_str!` now, so a file
//! outside `include` stops the compile rather than the consumer's test run.
//! That fixes the guards that use it; this closes the way around it. No
//! shipped source resolves a repository path at run time at all, so the next
//! guard cannot reintroduce the defect by reaching for `std::fs` directly.

use std::path::{Path, PathBuf};

/// Every `.rs` file under `dir`, recursively.
fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn no_shipped_source_resolves_a_repository_path() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut sources = vec![root.join("build.rs")];
    rust_sources(&root.join("src"), &mut sources);
    rust_sources(&root.join("examples"), &mut sources);
    assert!(
        sources.len() > 1,
        "found no shipped sources to scan; if they moved, so must this test"
    );

    // The marker, spelled so this file does not match its own scan.
    let marker = format!("CARGO_MANIFEST{}", "_DIR");
    let offenders: Vec<String> = sources
        .iter()
        .filter(|path| {
            std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
                .contains(&marker)
        })
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "these ship with the crate and resolve a path in the repository they \
         were built from, which a consumer does not have: {offenders:?}. Bake \
         the text in with include_str! (and add the path to Cargo.toml's \
         include list), or move the guard to tests/, which is not packaged."
    );
}
