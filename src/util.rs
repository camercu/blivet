//! Shared utility functions used across multiple modules.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// True when both paths name one file, whether or not it exists yet.
///
/// An existing file is settled by `canonicalize` on both operands. A file that
/// does not exist yet cannot be canonicalized — and for the overlap checks that
/// is the normal case, since a first run creates its pidfile and its log — so
/// the comparison resolves the *parent directory*, which does exist, and
/// compares `(canonical parent, file name)`. Two spellings of one parent (`..`
/// round trips, a symlinked directory such as `/var/run` -> `/run`) therefore
/// come out equal instead of slipping through as distinct paths.
///
/// Only when a path has no file name, or its parent cannot be resolved either,
/// does this fall back to byte equality. The fallback set is strictly narrower
/// than the comparison it replaces: any pair that was byte-equal has an equal
/// parent and an equal file name, so nothing that used to compare equal stops
/// doing so.
pub(crate) fn paths_same(a: &Path, b: &Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        return ca == cb;
    }
    match (resolve_in_parent(a), resolve_in_parent(b)) {
        (Some(ra), Some(rb)) => ra == rb,
        _ => a == b,
    }
}

/// A path as `(canonical parent directory, file name)`, or `None` when it names
/// no file or its parent does not exist.
fn resolve_in_parent(path: &Path) -> Option<(PathBuf, &OsStr)> {
    let name = path.file_name()?;
    let parent = match path.parent() {
        // A bare file name is relative to the working directory.
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        None => return None,
    };
    Some((std::fs::canonicalize(parent).ok()?, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlink_and_target_are_canonically_equal() {
        // Distinguishes canonicalization from byte comparison: the spellings
        // differ (`Path` equality would say false) but resolve to one file.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a");
        std::fs::write(&target, "x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(paths_same(&target, &link));
    }

    #[test]
    fn existing_distinct_files_differ() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "x").unwrap();
        assert!(!paths_same(&a, &b));
    }

    // Covers: R115
    #[test]
    fn not_yet_created_file_named_two_ways_is_one_file() {
        // The first run of any daemon: neither the pidfile nor the stdout file
        // exists yet, so neither operand canonicalizes. The parent does exist —
        // `validate_parent_writable` has already proved it — and that is what
        // makes the two spellings comparable.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("run");
        std::fs::create_dir(&sub).unwrap();

        let plain = sub.join("daemon.pid");
        let dotdot = sub.join("..").join("run").join("daemon.pid");
        assert!(paths_same(&plain, &dotdot));
    }

    // Covers: R115
    #[test]
    fn not_yet_created_file_under_a_symlinked_parent_is_one_file() {
        // The /var/run -> /run shape.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("run");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("varrun");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(paths_same(
            &real.join("daemon.pid"),
            &link.join("daemon.pid")
        ));
    }

    // Covers: R115
    #[test]
    fn not_yet_created_distinct_files_in_one_directory_differ() {
        // The widening must not swallow genuinely distinct paths.
        let dir = tempfile::tempdir().unwrap();
        assert!(!paths_same(
            &dir.path().join("daemon.pid"),
            &dir.path().join("daemon.log")
        ));
    }

    #[test]
    fn nonexistent_paths_fall_back_to_byte_equality() {
        assert!(paths_same(
            Path::new("/nonexistent/a"),
            Path::new("/nonexistent/a")
        ));
        assert!(!paths_same(
            Path::new("/nonexistent/a"),
            Path::new("/nonexistent/b")
        ));
    }
}
