//! Shared utility functions used across multiple modules.

use std::path::Path;

/// Compare two paths using canonicalize() with byte-equal fallback.
pub(crate) fn paths_same(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
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
