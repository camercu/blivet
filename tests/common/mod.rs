//! Helpers shared by the guards that scan the crate's own sources.
//!
//! Integration tests are separate crates, so a helper used by more than one of
//! them lives here rather than being copied — two copies of a source scan are
//! two things that have to be corrected together when the scan is wrong.
//!
//! Each of those crates compiles this module in full and uses a subset of it,
//! so an unused item here reports which crate included it, not that the item
//! is dead. `tests/helpers/mod.rs` carries the same allowance for the same
//! reason.
#![allow(dead_code)]

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

/// The code part of a Rust `line`: everything before a trailing `//`.
pub fn code_of(line: &str) -> &str {
    code_before(line, "//")
}

/// The code part of `line`, where `marker` begins a comment.
///
/// A guard that scans sources judges a line on its code alone. Otherwise
/// `let d = tmp_dir(); // not /tmp` is accused of the very thing it avoids, and
/// no comment could name the rule it documents. A whole-line comment yields the
/// empty string and so matches nothing.
///
/// A `marker` inside a string literal ends the scan early. That is a hole
/// rather than a hazard: it exempts a line, it never accuses one.
pub fn code_before<'a>(line: &'a str, marker: &str) -> &'a str {
    line.split_once(marker).map_or(line, |(code, _)| code)
}
