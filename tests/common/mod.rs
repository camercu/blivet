//! Helpers shared by the guards that scan the crate's own sources.
//!
//! Integration tests are separate crates, so a helper used by more than one of
//! them lives here rather than being copied — two copies of a source scan are
//! two things that have to be corrected together when the scan is wrong.

use std::path::{Path, PathBuf};

/// Recursively collect `.rs` files under `dir`.
///
/// A missing directory contributes nothing, so a caller may name a directory
/// that need not exist.
pub fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
