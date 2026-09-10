//! Guard: one place re-invokes the test binary, and it captures the child.
//!
//! A test whose effects cannot be undone in-process re-runs itself in a child
//! process. A child that inherits the parent's stdout writes into the middle of
//! it, alongside the other children running in parallel, which can split a line
//! so that whatever reads the run's summary lines no longer matches one. The
//! child also prints a summary line of its own, and `scripts/assert-tests-ran.sh`
//! counts those. `test_support::rerun_in_subprocess` captures the child and
//! reports its streams only when it failed, so every re-invocation goes through
//! that one helper and no other library source names `current_exe`.

use std::path::PathBuf;

mod common;
use common::{code_of, rust_files};

/// The one library source allowed to re-invoke the test binary.
const OWNER: &str = "test_support.rs";

#[test]
fn only_test_support_re_invokes_the_test_binary() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();

    let mut offenders = Vec::new();
    for file in files {
        if file.file_name().is_some_and(|n| n == OWNER) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel = file.strip_prefix(&root).unwrap_or(&file).display();
        for (i, line) in text.lines().enumerate() {
            if code_of(line).contains("current_exe") {
                offenders.push(format!("  {rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "the test binary is re-invoked outside `{OWNER}`:\n{}\n\
         Call `test_support::rerun_in_subprocess`, which captures the child's \
         output instead of letting it interleave with the parent's.",
        offenders.join("\n")
    );
}
