//! Guard: every line-pinned mutant exclusion still names a cfg-dead item.
//!
//! `.cargo/mutants.toml` excludes two mutants by file and line, because each
//! one's target shares a name and a signature with the live implementation it
//! stands in for — the mutant's description cannot tell them apart, so the
//! position is the only thing that can.
//!
//! A line pin goes stale the moment the file above it grows, and the pins it
//! replaces had: they read `36\d` while the items had moved to 358 and 426, so
//! the exclusions had stopped excluding and nothing said so. cargo-mutants
//! itself would only have reported the survivors as noise in a sweep that is
//! not part of any gate. This is the backstop the comment used to ask a reader
//! to be.

use std::path::PathBuf;

/// How many source lines above the pin the gating attribute may sit.
///
/// The item's doc comment and its `#[deprecated]` note come between them.
const ATTRIBUTE_WINDOW: usize = 16;

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `file:line` a mutant exclusion pins, as written in the config.
fn pinned_positions(config: &str) -> Vec<(String, usize)> {
    let mut pins = Vec::new();
    for line in config.lines() {
        let code = line.split('#').next().unwrap_or_default();
        let Some(at) = code.find("src/") else {
            continue;
        };
        let rest = &code[at..];
        let Some((path, tail)) = rest.split_once(".rs") else {
            continue;
        };
        // The config is a regex, so the dot before `rs` is escaped.
        let path = format!("{}.rs", path.trim_end_matches('\\'));
        let number: String = tail
            .trim_start_matches(':')
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(line_number) = number.parse::<usize>() {
            pins.push((path, line_number));
        }
    }
    pins
}

#[test]
fn every_pinned_exclusion_names_a_cfg_dead_item() {
    let pins = pinned_positions(&read(".cargo/mutants.toml"));
    assert!(
        !pins.is_empty(),
        "parsed no line-pinned exclusions; if the pins are gone, so is this test"
    );

    for (file, line_number) in pins {
        let source = read(&file);
        let lines: Vec<&str> = source.lines().collect();
        assert!(
            line_number <= lines.len(),
            "{file}:{line_number} is past the end of the file ({} lines)",
            lines.len()
        );
        // Lines are 1-based, and the attribute sits above the item.
        let start = line_number.saturating_sub(ATTRIBUTE_WINDOW + 1);
        let window = &lines[start..line_number - 1];
        assert!(
            window
                .iter()
                .any(|l| l.trim_start().starts_with("#[cfg(not(")),
            "{file}:{line_number} is pinned as a cfg-dead stub, but no \
             `#[cfg(not(...))]` sits above it — the pin has drifted off the \
             item it was written for, and the exclusion now spares nothing \
             (or the wrong thing). Context:\n{}",
            window.join("\n")
        );
    }
}
