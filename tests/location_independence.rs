//! Guard: no shipped Rust source names a fixed filesystem location.
//!
//! `/tmp` is not universal. Android and Termux have no `/tmp` at all, and a
//! sandbox can point `TMPDIR` anywhere. A test that hardcodes the path fails
//! there for a reason unrelated to what it asserts, which is the worst kind of
//! red: it accuses the code of a defect the code does not have.
//!
//! Whether a given literal actually reaches the disk is not visible at the call
//! site — `c.pidfile("/tmp")` before `validate()` stats the real path, while
//! `PathBuf::from("/tmp/out.log")` in a plan test is an opaque string. Since
//! the safe and unsafe uses look identical, the path is banned outright from
//! code. A test that needs a real directory calls `test_support::tmp_dir()`,
//! which honours `TMPDIR`; a test whose path is only a value names a plainly
//! fictional one such as `/a/x.pid`; prose examples name a real daemon
//! location. Line comments and doc comments may still discuss `/tmp` by name.
//! A block comment cannot: the scan reads a line at a time and cannot tell it
//! is inside one.

use std::path::PathBuf;

mod common;
use common::rust_files;

/// This file, which is exempt from its own scan.
const THIS_FILE: &str = "location_independence.rs";

#[test]
fn no_source_file_hardcodes_tmp() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("tests"), &mut files);
    // Examples are the code a new user copies, so they must run where the
    // crate claims support — Termux included.
    rust_files(&root.join("examples"), &mut files);
    // build.rs ships with the crate and is source like any other.
    files.push(root.join("build.rs"));
    files.sort();

    let mut offenders = Vec::new();
    for file in files {
        // The rule has to name the path it bans, in the scan and in the
        // failure message both, so it exempts the file that states it.
        if file.file_name().is_some_and(|n| n == THIS_FILE) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel = file.strip_prefix(&root).unwrap_or(&file).display();
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            // A whole-line comment is prose about the rule, not a use of it.
            if trimmed.starts_with("//") {
                continue;
            }
            // A trailing comment is prose about the rule too, so the line is
            // judged on its code alone — otherwise `let d = tmp_dir(); // not
            // /tmp` is accused of hardcoding a path it does not use.
            let code = trimmed.split("//").next().unwrap_or(trimmed);
            if code.contains("/tmp") {
                offenders.push(format!("  {rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "hardcoded /tmp path(s):\n{}\n\
         Call `test_support::tmp_dir()` when the path reaches the filesystem; \
         otherwise name a path that is not a temp directory.",
        offenders.join("\n")
    );
}
