//! The parts of `README.md` and `docs/SPEC.md` that a machine owns.
//!
//! Two facts in those documents are derived from somewhere else and change
//! whenever that somewhere else does: the platforms the capability table in
//! `build.rs` grants, and the MSRV in `Cargo.toml`. Prose that restates a
//! derived fact goes stale — that is what happened to the platform list, in
//! five copies, when Android was added.
//!
//! A guard that compared the prose with its source would catch the drift, but
//! a guard is the last rung: it parses wording, breaks on a reword, and freezes
//! the document's shape. Generation is the rung above it. This test owns those
//! cells and nothing else — every other word in both documents stays
//! hand-written, and [`rewrite`] preserves it byte for byte.
//!
//! - `cargo test --test docgen` fails when a committed document differs from
//!   what the source says it should be.
//! - `just docs-bless` rewrites the documents from the source.
//!
//! This lives in `tests/`, not in `src/`: it reads and writes repository files,
//! and a shipped source may not do that — `tests/packaging.rs` enforces it.

use std::path::{Path, PathBuf};

/// The documents with machine-owned cells in them.
const GENERATED_DOCS: &[&str] = &["README.md", "docs/SPEC.md"];

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The supported platforms, as the capability table in `build.rs` names them.
fn supported_platforms() -> String {
    env!("BLIVET_PLATFORMS")
        .split(',')
        .collect::<Vec<_>>()
        .join(", ")
}

/// `rust-version` from `Cargo.toml`, the one MSRV.
fn msrv() -> String {
    read(&repo("Cargo.toml"))
        .lines()
        .find_map(|line| line.trim().strip_prefix("rust-version = "))
        .expect("Cargo.toml declares rust-version")
        .trim_matches('"')
        .to_string()
}

/// The cells of a markdown table row, or `None` if `line` is not one.
fn table_cells(line: &str) -> Option<Vec<&str>> {
    let trimmed = line.trim_end();
    let inner = trimmed.strip_prefix('|')?.strip_suffix('|')?;
    Some(inner.split('|').collect())
}

/// `text` with every machine-owned cell set to what its source says.
///
/// Everything else is copied through unchanged, so blessing a document never
/// reflows prose, reorders a table, or touches a row this does not own.
fn rewrite(text: &str, platforms: &str, msrv: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_heading = "";
    for line in text.lines() {
        let rendered = if let Some(mut cells) = table_cells(line) {
            // The Supported row's last cell is the platform list; the tier
            // names and the "what CI proves" column are prose and stay.
            let supported = cells
                .first()
                .is_some_and(|tier| tier.trim().trim_matches('*') == "Supported");
            if supported && cells.len() >= 3 {
                let last = cells.len() - 1;
                cells[last] = "";
                let mut row = format!("|{}", cells.join("|"));
                row.push_str(&format!(" {platforms} |"));
                row
            } else {
                line.to_string()
            }
        } else if line.contains("img.shields.io/badge/MSRV-") {
            let (before, rest) = line.split_once("badge/MSRV-").expect("the badge");
            let (_, after) = rest.split_once('-').expect("the badge's colour");
            format!("{before}badge/MSRV-{msrv}-{after}")
        } else if previous_heading == "## Minimum supported Rust version" && !line.trim().is_empty()
        {
            msrv.to_string()
        } else {
            line.to_string()
        };

        if line.starts_with("## ") {
            previous_heading = line.trim_end();
        } else if !line.trim().is_empty() {
            previous_heading = "";
        }

        out.push_str(&rendered);
        out.push('\n');
    }
    out
}

/// Every machine-owned cell in the committed documents says what its source
/// says.
#[test]
fn generated_doc_cells_are_current() {
    let platforms = supported_platforms();
    let msrv = msrv();
    for doc in GENERATED_DOCS {
        let path = repo(doc);
        let committed = read(&path);
        assert_eq!(
            rewrite(&committed, &platforms, &msrv),
            committed,
            "{doc} is out of date with the source of the cells it does not own. \
             Run `just docs-bless`; do not edit those cells by hand"
        );
    }
}

/// Rewrite the machine-owned cells. Run by `just docs-bless`.
#[test]
#[ignore = "writes to the repository; run it with `just docs-bless`"]
fn bless_generated_doc_cells() {
    let platforms = supported_platforms();
    let msrv = msrv();
    for doc in GENERATED_DOCS {
        let path = repo(doc);
        let blessed = rewrite(&read(&path), &platforms, &msrv);
        std::fs::write(&path, blessed).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    println!("blessed: {}", GENERATED_DOCS.join(", "));
}
