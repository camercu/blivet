//! Guard: the library decides on capabilities, not on OS names.
//!
//! ADR 0001 (`docs/adr/0001-capability-based-platform-gating.md`) has
//! `build.rs` map each `target_os` to the capabilities it has, and the library
//! gate on those capability aliases. A `target_os` test in library code makes a
//! decision the table cannot see, so the next platform the table gains skips it
//! silently — the cliff the ADR exists to remove. It already happened once in
//! this repository: gating the procfs cwd lookup on `target_os = "linux"` sent
//! Android down a fallback that cannot answer there (`tests/helpers/mod.rs`).
//!
//! The ADR's one exception is `src/unsafe_ops.rs`, where an OS name selects the
//! *mechanism* a capability is implemented with and the table has no column
//! for it. Everything else under `src/` names no OS.
//!
//! `build.rs` is not scanned: it is the table. Tests and examples are not
//! scanned either — the rule is about what the library decides, and an example
//! a consumer copies has to enumerate OS names, since consumers cannot see this
//! crate's aliases.

use std::path::PathBuf;

mod common;
use common::{offending_lines, rust_files};

/// The one library file where an OS name may select a mechanism.
const MECHANISM_FILE: &str = "unsafe_ops.rs";

#[test]
fn library_code_gates_on_capabilities_not_os_names() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();

    let offenders = offending_lines(&root, &files, MECHANISM_FILE, |code| {
        code.contains("target_os")
    });

    assert!(
        offenders.is_empty(),
        "library code decides on an OS name:\n{}\n\
         Gate on a capability alias from build.rs's table instead, adding a \
         capability if none answers the question — see ADR 0001. An OS name \
         that only picks how a capability is implemented belongs in \
         src/{MECHANISM_FILE}.",
        offenders.join("\n")
    );
}
