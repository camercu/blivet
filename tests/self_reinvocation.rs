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
use common::{offending_lines, rust_files};

/// The one library source allowed to re-invoke the test binary.
const OWNER: &str = "test_support.rs";

#[test]
fn only_test_support_re_invokes_the_test_binary() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();

    let offenders = offending_lines(&root, &files, OWNER, |code| code.contains("current_exe"));

    assert!(
        offenders.is_empty(),
        "the test binary is re-invoked outside `{OWNER}`:\n{}\n\
         Call `test_support::rerun_in_subprocess`, which captures the child's \
         output instead of letting it interleave with the parent's.",
        offenders.join("\n")
    );
}
