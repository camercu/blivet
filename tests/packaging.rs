//! Guards over what ships to crates.io.
//!
//! `src/` ships with the crate and the rest of the repository does not, so a
//! `#[cfg(test)]` guard in `src/` runs on a packaged or vendored copy with
//! only `Cargo.toml`'s `include` list present. One of them read the justfile
//! through `CARGO_MANIFEST_DIR` and panicked for exactly that consumer.
//!
//! `src/doc_sync.rs` bakes its inputs in with `include_str!` now, so a file
//! outside `include` stops the compile rather than the consumer's test run.
//! That fixes the guards that use it; the two tests here close the ways around
//! it: naming the repository root through `CARGO_MANIFEST_DIR`, and naming a
//! repository file by a relative path that resolves only in a checkout.
//!
//! The set of files a consumer does not have is derived from `include`, so
//! packaging a file drops it from the scan with no edit here.

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

/// Every Rust source the packaged crate carries.
fn shipped_sources(root: &Path) -> Vec<PathBuf> {
    let mut sources = vec![root.join("build.rs")];
    rust_sources(&root.join("src"), &mut sources);
    rust_sources(&root.join("examples"), &mut sources);
    assert!(
        sources.len() > 1,
        "found no shipped sources to scan; if they moved, so must this test"
    );
    sources
}

/// `path` as the repository sees it, for a failure message.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[test]
fn no_shipped_source_resolves_a_repository_path() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = shipped_sources(&root);

    // The marker, spelled so this file does not match its own scan.
    let marker = format!("CARGO_MANIFEST{}", "_DIR");
    let offenders: Vec<String> = sources
        .iter()
        .filter(|path| {
            std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
                .contains(&marker)
        })
        .map(|path| relative(&root, path))
        .collect();

    assert!(
        offenders.is_empty(),
        "these ship with the crate and resolve a path in the repository they \
         were built from, which a consumer does not have: {offenders:?}. Bake \
         the text in with include_str! (and add the path to Cargo.toml's \
         include list), or move the guard to tests/, which is not packaged."
    );
}

/// The `include` patterns from `Cargo.toml`, with the leading `/` removed.
fn include_patterns(root: &Path) -> Vec<String> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("Cargo.toml");
    let list = manifest
        .split_once("include = [")
        .expect("Cargo.toml has an `include` list")
        .1;
    let list = list
        .split_once(']')
        .expect("the `include` list is closed")
        .0;
    let patterns: Vec<String> = list
        .split('"')
        .skip(1)
        .step_by(2)
        .map(|p| p.trim_start_matches('/').to_string())
        .collect();
    assert!(
        !patterns.is_empty(),
        "parsed no patterns out of Cargo.toml's `include` list; if its shape \
         changed, so must this parser"
    );
    patterns
}

/// Whether the packaged crate carries `rel` (a path relative to the repo root).
///
/// `Cargo.toml` and `Cargo.lock` are not in `include`; cargo packages them
/// regardless.
fn is_shipped(rel: &str, patterns: &[String]) -> bool {
    if rel == "Cargo.toml" || rel == "Cargo.lock" {
        return true;
    }
    patterns
        .iter()
        .any(|pattern| match pattern.split_once("/**") {
            Some((prefix, _)) => rel == prefix || rel.starts_with(&format!("{prefix}/")),
            None => rel == pattern,
        })
}

/// Repository paths a consumer of the packaged crate does not have.
///
/// A directory no pattern reaches into is reported whole and not descended
/// into: naming `scripts` is already the defect, and the files under it add
/// nothing but noise. A directory a pattern reaches into partially — `docs`,
/// of which only `SPEC.md` ships — is descended into, so the rest of it is
/// still reported.
fn unshipped_paths(dir: &Path, root: &Path, patterns: &[String], found: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        let rel = path
            .strip_prefix(root)
            .expect("an entry below the root")
            .to_string_lossy()
            .into_owned();
        if is_shipped(&rel, patterns) {
            continue;
        }
        let reached_into = patterns.iter().any(|p| p.starts_with(&format!("{rel}/")));
        if path.is_dir() && reached_into {
            unshipped_paths(&path, root, patterns, found);
        } else {
            found.push(rel);
        }
    }
}

/// Directories that are not part of the source tree: build output, tool state,
/// and the VCS. They are unshipped like any other, but a source naming one is
/// not the defect this guards against, and `target` alone is a word common
/// enough to fire on prose.
const NOT_SOURCE_TREE: &[&str] = &[
    ".git",
    ".harden",
    "mutants.out",
    "mutants.out.old",
    "node_modules",
    "target",
];

// The second way a shipped guard reaches a repository path: a relative
// literal, which resolves in a checkout and nowhere else.
#[test]
fn no_shipped_source_names_a_file_the_crate_does_not_ship() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let patterns = include_patterns(&root);
    let mut unshipped = Vec::new();
    unshipped_paths(&root, &root, &patterns, &mut unshipped);
    unshipped.retain(|rel| !NOT_SOURCE_TREE.contains(&rel.as_str()));
    assert!(
        unshipped.contains(&"justfile".to_string()),
        "expected the justfile to count as unshipped; if the `include` list or \
         the parser above changed, this scan may now be looking for nothing"
    );

    let mut offenders: Vec<String> = Vec::new();
    for path in shipped_sources(&root) {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for rel in &unshipped {
            // Quoted, so a comment that merely mentions the justfile is not an
            // offender; a path that reaches the filesystem is spelled as a
            // literal.
            if text.contains(&format!("\"{rel}\"")) {
                offenders.push(format!("{} names \"{rel}\"", relative(&root, &path)));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these ship with the crate and name a path only a checkout has, so a \
         consumer running the packaged tests gets a failure that is nothing to \
         do with them: {offenders:?}. Bake the text in with include_str! (and \
         add the path to Cargo.toml's include list), or move the guard to \
         tests/, which is not packaged."
    );
}
