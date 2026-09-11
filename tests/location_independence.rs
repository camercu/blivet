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
//! location. A directory that only ends in those same four characters is a
//! different path and stays allowed — `/data/local/tmp` on a device, `/var/tmp`
//! elsewhere, both of which this crate itself uses.
//! Line comments and doc comments may still discuss `/tmp` by name.
//! A block comment cannot: the scan reads a line at a time and cannot tell it
//! is inside one.

use std::path::PathBuf;

mod common;
use common::{code_of, rust_files};

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
            if names_tmp_root(code_of(line)) {
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

/// True when `code` names the absolute path `/tmp`.
///
/// The temp directories this crate standardises on end in those same four
/// characters — `/data/local/tmp` on a device, `/var/tmp` elsewhere — so a
/// plain substring test accuses the very paths the rule tells a caller to use.
fn names_tmp_root(code: &str) -> bool {
    const TMP: &str = "/tmp";

    /// True where `c` can be part of a directory name.
    ///
    /// One of these on either side of the match means a different path:
    /// `/var/tmp` before, `/tmpfs` after. Everything else terminates the name,
    /// so `/tmp/x`, `"/tmp"`, `format!("/tmp{i}")` and `"/tmp\0file"` are all
    /// the banned path. Stated as what may not sit there rather than what may:
    /// a list of permitted terminators has to anticipate every way a path gets
    /// written, and the one written here missed three.
    fn extends_a_name(c: char) -> bool {
        c.is_alphanumeric() || matches!(c, '.' | '-' | '_')
    }

    code.match_indices(TMP).any(|(at, _)| {
        let starts_the_path = !code[..at].chars().next_back().is_some_and(extends_a_name);
        let ends_the_name = !code[at + TMP.len()..]
            .chars()
            .next()
            .is_some_and(extends_a_name);
        starts_the_path && ends_the_name
    })
}

/// This file is exempt from its own scan, so it may name the paths it judges.
#[test]
fn the_banned_path_is_named_however_it_is_written() {
    assert!(names_tmp_root(r#"c.pidfile("/tmp")"#));
    assert!(names_tmp_root(r#"PathBuf::from("/tmp/out.log")"#));
    // An interpolated path is the natural way to make one unique per test.
    assert!(names_tmp_root(r#"let p = format!("/tmp{i}.pid");"#));
    assert!(names_tmp_root(r#"let p = format!("/tmp{}", n);"#));
    // The literal this crate removed from its own sources, escape and all.
    assert!(names_tmp_root(r#"config.pidfile("/tmp\0file");"#));
}

#[test]
fn a_temp_directory_that_only_ends_in_tmp_is_not_the_banned_path() {
    assert!(!names_tmp_root(r#"let dir = "/data/local/tmp/blivet";"#));
    assert!(!names_tmp_root(
        r#"let dir = PathBuf::from("/var/tmp/x.pid");"#
    ));
    assert!(!names_tmp_root(r#"let fs = "/tmpfs";"#));
}
