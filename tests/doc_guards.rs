//! Guards over docs that restate source and cannot be generated from it.
//!
//! Most doc drift in this crate is now closed a rung higher: the front page's
//! platform table is written by `build.rs`, and `tests/docgen.rs` owns the
//! derived cells of `README.md` and `docs/SPEC.md`. What is left here is the
//! case neither rung reaches — example source a consumer copies, which has to
//! spell out what it spells out.
//!
//! These live in `tests/` rather than in the library. They check the
//! repository, not the platform, so a cross-built tier gains nothing by
//! running them, and a guard outside the shipped sources cannot reach for a
//! repository path that a consumer of the packaged crate does not have.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// The text of a repository file, read from the checkout.
///
/// Safe to do here and nowhere in `src/`: `tests/` is not packaged, so this
/// runs only where the repository exists.
fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The front page shows consumers how to gate the `daemonize` call for an
/// exotic target. Consumers cannot see this crate's capability aliases, so that
/// example spells out `target_os` — a copy of the table that would otherwise
/// go stale the next time a platform is added.
///
/// The example carries the list twice, once for the checked call and once for
/// the `not(...)` fallback, and both have to move together: a platform added to
/// only the positive list leaves an example in which both branches are live on
/// that target. So each branch is compared with the table on its own.
///
/// Per branch, not per file: counting `target_os` occurrences over the whole
/// file says "the file mentions each name twice", which is a different claim.
/// It goes red on the first genuine platform `cfg` the file grows, and it
/// passes vacuously when a name dropped from one branch is restored to the
/// count by an unrelated `cfg` elsewhere — the very drift this exists to catch.
#[test]
fn front_page_cfg_example_lists_every_target_os() {
    // Both files spell the list out for consumers, who cannot see this crate's
    // aliases: the front page teaches the pattern and the example runs it.
    // Each is a copy of the table, so each is pinned to it.
    let table: BTreeSet<&str> = env!("BLIVET_SUPPORTED_TARGET_OS").split(',').collect();
    for doc in ["src/lib.rs", "examples/echo_server.rs"] {
        let text = read(doc);
        let text = text.as_str();
        // The fallback opener is searched for first and is the longer literal,
        // so the checked branch's opener cannot match inside it.
        for opener in ["#[cfg(not(any(", "#[cfg(any("] {
            let branch = cfg_branch(text, opener, doc);
            assert_eq!(
                target_os_clauses(branch)
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                table,
                "{doc}'s `{opener}` branch must name exactly the platforms the \
                 capability table does — a name missing here leaves the example \
                 with both branches live on that target, and a name the table \
                 lacks sends a reader to the `#[deprecated]` stub that panics"
            );
        }
    }
}

/// The text of the `cfg` clause list that `opener` opens.
fn cfg_branch<'a>(text: &'a str, opener: &str, doc: &str) -> &'a str {
    let at = text
        .find(opener)
        .unwrap_or_else(|| panic!("{doc}'s `cfg` example has no `{opener}` branch"));
    let rest = &text[at + opener.len()..];
    let end = rest
        .find("))")
        .unwrap_or_else(|| panic!("{doc}'s `{opener}` branch is never closed"));
    &rest[..end]
}

/// Every distinct `target_os` a `cfg` clause in `text` names.
fn target_os_clauses(text: &str) -> Vec<&str> {
    const CLAUSE: &str = "target_os = \"";
    let mut names = Vec::new();
    for (at, _) in text.match_indices(CLAUSE) {
        let rest = &text[at + CLAUSE.len()..];
        if let Some(end) = rest.find('"') {
            let name = &rest[..end];
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}
