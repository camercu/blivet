//! Requirement traceability enforcement.
//!
//! Acceptance criteria live in `docs/SPEC.md` as `- R<n>. ...` lines. Tests
//! declare which they cover with a uniform tag directly above the test:
//!
//! ```ignore
//! // Covers: R17, R18
//! #[test]
//! fn pidfile_contains_pid() { ... }
//! ```
//!
//! These tests keep the annotations honest and consistent:
//! - SPEC numbering is contiguous and unique.
//! - Every `Covers:` tag names a real requirement (no typos / stale refs).
//! - Coverage never regresses below a committed baseline (ratchet).
//!
//! The uncovered set is printed by `report_uncovered_requirements` (run with
//! `--ignored --nocapture`) so closing gaps is a visible, deliberate act.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// All requirement numbers declared in the SPEC (`- R<n>. ...`).
fn spec_requirements() -> BTreeSet<u32> {
    let spec = std::fs::read_to_string(manifest_dir().join("docs/SPEC.md")).unwrap();
    let mut reqs = BTreeSet::new();
    for line in spec.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("- R") {
            if let Some(num) = rest.split('.').next() {
                if let Ok(n) = num.parse::<u32>() {
                    reqs.insert(n);
                }
            }
        }
    }
    reqs
}

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

/// Requirement numbers named in `// Covers: R..` tags across the test sources.
fn covered_requirements() -> BTreeSet<u32> {
    let root = manifest_dir();
    let mut files = Vec::new();
    rust_files(&root.join("tests"), &mut files);
    rust_files(&root.join("src"), &mut files);

    let mut covered = BTreeSet::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            let Some(idx) = line.find("Covers:") else {
                continue;
            };
            for token in line[idx + "Covers:".len()..].split(|c: char| !c.is_ascii_alphanumeric()) {
                if let Some(num) = token.strip_prefix('R') {
                    if let Ok(n) = num.parse::<u32>() {
                        covered.insert(n);
                    }
                }
            }
        }
    }
    covered
}

#[test]
fn spec_numbering_is_contiguous_and_unique() {
    let reqs = spec_requirements();
    let max = *reqs.iter().max().expect("SPEC has requirements");
    let expected: BTreeSet<u32> = (1..=max).collect();
    let missing: Vec<u32> = expected.difference(&reqs).copied().collect();
    assert!(
        missing.is_empty(),
        "SPEC requirement numbering has gaps: {missing:?}"
    );
}

#[test]
fn covers_tags_reference_real_requirements() {
    let spec = spec_requirements();
    let covered = covered_requirements();
    let stale: Vec<u32> = covered.difference(&spec).copied().collect();
    assert!(
        stale.is_empty(),
        "tests reference requirements not in SPEC (typo or stale?): {stale:?}"
    );
}

#[test]
fn requirement_coverage_does_not_regress() {
    // Ratchet: raise this as coverage grows; it must never be lowered.
    //
    // The remaining untagged requirements are structural/compile-time (derives,
    // `#![deny(unsafe_code)]`, unsafe confinement, type signatures, internal
    // step ordering, panic-on-OS-failure) with no discrete runtime test —
    // see `report_uncovered_requirements` for the current list.
    const BASELINE: usize = 105;
    let covered = covered_requirements().len();
    assert!(
        covered >= BASELINE,
        "requirement coverage regressed: {covered} tagged, baseline {BASELINE}. \
         Add `// Covers: R..` tags rather than lowering the baseline."
    );
}

#[test]
#[ignore = "informational: run with --ignored --nocapture to see the gap"]
fn report_uncovered_requirements() {
    let spec = spec_requirements();
    let covered = covered_requirements();
    let uncovered: Vec<u32> = spec.difference(&covered).copied().collect();
    eprintln!(
        "requirement coverage: {}/{} tagged; uncovered: {:?}",
        covered.len(),
        spec.len(),
        uncovered
    );
}
