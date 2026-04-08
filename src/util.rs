//! Shared utility functions used across multiple modules.

use std::path::Path;

/// Compare two paths using canonicalize() with byte-equal fallback.
pub(crate) fn paths_same(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}
