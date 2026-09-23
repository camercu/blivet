//! Requirement traceability enforcement.
//!
//! Acceptance criteria live in `docs/SPEC.md` as `- R<n>. ...` lines. Tests
//! declare which they cover with a `// Covers:` line comment on the test —
//! above it, or among its attributes:
//!
//! ```ignore
//! // Covers: R17, R18
//! #[test]
//! fn pidfile_contains_pid() { ... }
//! ```
//!
//! These tests keep the annotations honest and consistent:
//! - SPEC numbering is contiguous and unique.
//! - A tag counts only on a test function, so a tag in prose, in a string, or
//!   left behind by a deleted test covers nothing.
//! - Every `Covers:` tag names a real requirement (no typos / stale refs).
//! - Coverage never regresses below a committed baseline (ratchet).
//!
//! The uncovered set is printed by `report_uncovered_requirements` (run with
//! `--ignored --nocapture`) so closing gaps is a visible, deliberate act.

use std::collections::BTreeSet;
use std::path::PathBuf;

mod common;
use common::rust_files;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every requirement number the SPEC declares (`- R<n>. ...`), in the order it
/// declares them, repeats included.
///
/// A set would collapse a repeat before anything could see it, and a repeat is
/// the one thing [`spec_numbering_is_contiguous_and_unique`] exists to catch:
/// two `- R42.` lines make every `// Covers: R42` tag ambiguous, and the
/// coverage ratchet counts the pair once.
fn spec_requirement_numbers() -> Vec<u32> {
    let spec = std::fs::read_to_string(manifest_dir().join("docs/SPEC.md")).unwrap();
    let mut reqs = Vec::new();
    for line in spec.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("- R") {
            if let Some(num) = rest.split('.').next() {
                if let Ok(n) = num.parse::<u32>() {
                    reqs.push(n);
                }
            }
        }
    }
    reqs
}

/// The distinct requirement numbers the SPEC declares.
fn spec_requirements() -> BTreeSet<u32> {
    spec_requirement_numbers().into_iter().collect()
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
        covered.extend(covering_tags(&text));
    }
    covered
}

/// The requirement numbers one source file's `Covers:` tags name.
///
/// A tag counts only when it is a plain `// Covers:` line comment on a test: it
/// shares one block of attributes and comments with a `fn` that carries a test
/// attribute. So a tag in documentation, in a string, above a helper, or left
/// behind by a deleted test names nothing — typing a number is not covering it.
fn covering_tags(text: &str) -> BTreeSet<u32> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut covered = BTreeSet::new();
    for (at, line) in lines.iter().enumerate() {
        let Some(list) = line.strip_prefix("// Covers:") else {
            continue;
        };
        if !tags_a_test(&lines, at) {
            continue;
        }
        for token in list.split(|c: char| !c.is_ascii_alphanumeric()) {
            if let Some(num) = token.strip_prefix('R') {
                if let Ok(n) = num.parse::<u32>() {
                    covered.insert(n);
                }
            }
        }
    }
    covered
}

/// Whether the tag on line `tag` belongs to a test function.
///
/// The tag, the item's attributes and any comments form one run of lines with
/// nothing else between them, so the run is walked both ways from the tag: the
/// attributes may sit above it or below it. The run must end in a `fn`, and one
/// of its attributes must mark that `fn` as a test.
fn tags_a_test(lines: &[&str], tag: usize) -> bool {
    let in_run = |l: &&str| l.is_empty() || l.starts_with("//") || l.starts_with("#[");
    let is_test_attr = |l: &&str| *l == "#[test]" || l.contains("::test]") || l.contains("::test(");

    let above = lines[..tag].iter().rev().take_while(|l| in_run(l));
    let rest = &lines[tag + 1..];
    let run_below = rest.iter().take_while(|l| in_run(l)).count();
    let item = rest.get(run_below).copied().unwrap_or_default();

    is_fn(item) && above.chain(&rest[..run_below]).any(is_test_attr)
}

/// Whether `line` opens a function item.
fn is_fn(line: &str) -> bool {
    let mut line = line;
    for qualifier in ["pub(crate) ", "pub ", "async ", "unsafe "] {
        line = line.strip_prefix(qualifier).unwrap_or(line);
    }
    line.starts_with("fn ")
}

#[test]
fn spec_numbering_is_contiguous_and_unique() {
    let numbers = spec_requirement_numbers();
    let reqs: BTreeSet<u32> = numbers.iter().copied().collect();

    let mut repeated: Vec<u32> = numbers
        .iter()
        .filter(|n| numbers.iter().filter(|m| m == n).count() > 1)
        .copied()
        .collect();
    repeated.dedup();
    assert!(
        repeated.is_empty(),
        "SPEC declares requirement number(s) more than once, so every \
         `// Covers:` tag naming one is ambiguous: {repeated:?}"
    );

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
    // `report_uncovered_requirements` lists what remains. Read each one before
    // deciding it has no runtime test: some that looked structural turned out
    // to be behaviours a test already observed, untagged.
    const BASELINE: usize = 125;
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

// ---- the tag parser, on the shapes it must accept and reject ----

fn tags(text: &str) -> Vec<u32> {
    covering_tags(text).into_iter().collect()
}

#[test]
fn a_tag_above_a_test_counts() {
    assert_eq!(tags("// Covers: R3, R4\n#[test]\nfn t() {}\n"), [3, 4]);
}

#[test]
fn a_tag_among_the_test_attributes_counts() {
    // src/steps.rs writes its tag between the attributes and the fn.
    let text = "#[test]\n#[serial]\n// Covers: R9\nfn t() {}\n";
    assert_eq!(tags(text), [9]);
}

#[test]
fn a_tag_in_documentation_does_not_count() {
    // This file's own module docs show the convention inside an ignored
    // fence; showing a tag is not covering a requirement.
    let text = "//! ```ignore\n//! // Covers: R17\n//! #[test]\n//! fn t() {}\n//! ```\n";
    assert_eq!(tags(text), Vec::<u32>::new());
}

#[test]
fn a_tag_above_a_function_that_is_not_a_test_does_not_count() {
    assert_eq!(tags("// Covers: R5\nfn helper() {}\n"), Vec::<u32>::new());
}

#[test]
fn a_tag_whose_test_was_deleted_does_not_count() {
    // The comment outlived the test: the next item is something else.
    let text = "// Covers: R6\n\nconst X: u8 = 0;\n\n#[test]\nfn t() {}\n";
    assert_eq!(tags(text), Vec::<u32>::new());
}

#[test]
fn a_tag_inside_a_string_does_not_count() {
    assert_eq!(
        tags("#[test]\nfn t() {\n    let _ = \"// Covers: R7\";\n}\n"),
        Vec::<u32>::new()
    );
}
