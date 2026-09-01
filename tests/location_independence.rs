//! Guard: the test suite names no fixed filesystem location.
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
//! location. Comments and doc comments may still discuss `/tmp` by name.

use std::path::{Path, PathBuf};

/// This file, which is exempt from its own scan.
const THIS_FILE: &str = "location_independence.rs";

/// Recursively collect `.rs` files under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
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

#[test]
fn no_source_file_hardcodes_tmp() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("tests"), &mut files);
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
            if line.contains("/tmp") {
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
