//! Tests for `scripts/privileged-test.sh`, the privileged container tier.
//!
//! The script asserts what the tier claims: that it is root, and that the run
//! left nothing out. Those assertions were exercised only by running the real
//! container, which takes the happy path and so proves nothing about either.
//!
//! Both are reached here by putting stand-ins for `id` and `cargo` ahead of
//! `PATH`: the script's logic is what is under test, not cargo's.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;
use common::{code_of, rust_files};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Writes an executable stand-in for `name` that runs `body`.
fn stub(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write the stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("make it run");
}

/// Runs the tier script with `dir` ahead of `PATH`.
fn run_tier(dir: &Path) -> Output {
    let path = std::env::var("PATH").unwrap_or_default();
    std::process::Command::new("sh")
        .arg("scripts/privileged-test.sh")
        .current_dir(repo_root())
        .env("PATH", format!("{}:{path}", dir.display()))
        .output()
        .expect("the tier script runs")
}

/// Where `cargo_reporting`'s stand-in records each command line it was handed.
const ARGV_LOG: &str = "cargo-argv";

/// A `cargo` that reports a run of `count` tests with `skipped` left out,
/// answering as whichever harness the script asked for.
///
/// It also appends every command line it receives to [`ARGV_LOG`], so a test
/// can assert which flags the tier asked for. The stand-in cannot answer
/// differently per flag — it reports the same summary whatever it is handed —
/// so the flags are invisible to the other tests here.
fn cargo_reporting(dir: &Path, count: &str, skipped: &str) {
    stub(
        dir,
        "cargo",
        &format!(
            "printf '%s\\n' \"$*\" >> \"$(dirname \"$0\")/{ARGV_LOG}\"\n\
             if [ \"$1\" = nextest ]; then\n  \
               printf '   Summary [   0.1s] {count} tests run: {count} passed, {skipped} skipped\\n'\n\
             else\n  \
               printf 'test result: ok. 16 passed; 0 failed; 2 ignored\\n'\n\
             fi\n"
        ),
    );
}

/// Every command line [`cargo_reporting`]'s stand-in was handed, in order.
fn cargo_command_lines(dir: &Path) -> Vec<String> {
    let log = dir.join(ARGV_LOG);
    let text = std::fs::read_to_string(&log)
        .unwrap_or_else(|e| panic!("the stand-in recorded nothing in {}: {e}", log.display()));
    text.lines().map(str::to_owned).collect()
}

fn combined(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_tier_that_is_not_root_is_refused() {
    // GIVEN the tier running unprivileged
    let dir = tempfile::tempdir().unwrap();
    stub(dir.path(), "id", "echo 1000\n");
    cargo_reporting(dir.path(), "311", "0");

    let out = run_tier(dir.path());

    // THEN it refuses rather than reporting the passes of tests that skip
    // themselves when they cannot switch user.
    let text = combined(&out);
    assert!(
        !out.status.success(),
        "a non-root tier must fail; got:\n{text}"
    );
    assert!(
        text.contains("must run as root"),
        "the tier must say why it refused; got:\n{text}"
    );
}

#[test]
fn a_tier_that_skipped_tests_is_refused() {
    // GIVEN a root run that left tests out — the shape of a lost
    // `--run-ignored all`, where the privileged corpus never runs
    let dir = tempfile::tempdir().unwrap();
    stub(dir.path(), "id", "echo 0\n");
    cargo_reporting(dir.path(), "276", "35");

    let out = run_tier(dir.path());

    let text = combined(&out);
    assert!(
        !out.status.success(),
        "a run that skipped 35 tests must fail; got:\n{text}"
    );
    assert!(
        text.contains("35 test(s) were skipped"),
        "the tier must name the skip count; got:\n{text}"
    );
}

#[test]
fn a_root_run_that_left_nothing_out_passes() {
    // GIVEN the tier as the Dockerfile runs it
    let dir = tempfile::tempdir().unwrap();
    stub(dir.path(), "id", "echo 0\n");
    cargo_reporting(dir.path(), "311", "0");

    let out = run_tier(dir.path());

    assert!(
        out.status.success(),
        "a root run that skipped nothing must pass; got:\n{}",
        combined(&out)
    );
}

#[test]
fn the_tier_asks_for_the_ignored_corpus() {
    // GIVEN the tier as the Dockerfile runs it
    let dir = tempfile::tempdir().unwrap();
    stub(dir.path(), "id", "echo 0\n");
    cargo_reporting(dir.path(), "311", "0");

    let out = run_tier(dir.path());
    assert!(
        out.status.success(),
        "the tier must reach its cargo invocations; got:\n{}",
        combined(&out)
    );

    // THEN the test run asked for the ignored tests. Dropping the flag is
    // caught in the real container by the skip count, but the count only
    // moves because the flag is there, so nothing below this tier's own
    // script pins the two together.
    let lines = cargo_command_lines(dir.path());
    let nextest = lines
        .iter()
        .find(|line| line.starts_with("nextest "))
        .unwrap_or_else(|| {
            panic!(
                "the tier ran no nextest command; it ran:\n{}",
                lines.join("\n")
            )
        });
    assert!(
        nextest.contains("--run-ignored all"),
        "the privileged tier exists to run the ignored corpus, so its nextest \
         command must ask for it; it asked for:\n{nextest}"
    );
}

// ---- a test run as root must still test something ----

/// Calls that answer "is this process privileged?".
const PRIVILEGE_CHECKS: [&str; 3] = ["geteuid()", "is_root()", "is_root_on_linux()"];

/// Every early `return` guarded by a privilege check, in a test the ordinary
/// tiers run, as `  <path>:<line>: <condition>`.
///
/// The privileged tier runs the whole suite as root and counts what passed. A
/// test that returns when it finds itself root is counted as a pass there
/// having asserted nothing, and `--no-skips` cannot see it: an early return is
/// not a skip. The honest forms are an assertion that holds for either
/// identity, or a subprocess that sheds root and tests the same path.
///
/// `#[ignore]` tests are exempt: only the privileged tier runs them, and its
/// script refuses to run unless it is root, so a return that fires when it is
/// not root cannot fire there.
fn privilege_returns_in_tests(root: &Path, files: &[PathBuf]) -> Vec<String> {
    let mut offenders = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for (at, line) in lines.iter().enumerate() {
            let code = code_of(line).trim();
            let guards_on_privilege = code.starts_with("if ")
                && code.ends_with('{')
                && PRIVILEGE_CHECKS.iter().any(|c| code.contains(c));
            if !guards_on_privilege || !block_returns(&lines, at) || !in_ordinary_test(&lines, at) {
                continue;
            }
            let rel = file.strip_prefix(root).unwrap_or(file).display();
            offenders.push(format!("  {rel}:{}: {code}", at + 1));
        }
    }
    offenders
}

/// Whether the block opened on line `open` returns before it closes.
fn block_returns(lines: &[&str], open: usize) -> bool {
    let indent = lines[open].len() - lines[open].trim_start().len();
    lines[open + 1..]
        .iter()
        .take_while(|l| {
            let close = l.len() - l.trim_start().len() == indent && l.trim_start().starts_with('}');
            !close
        })
        .any(|l| code_of(l).trim_start().starts_with("return"))
}

/// Whether line `at` sits in a test the ordinary tiers run: a function marked
/// `#[test]` and not `#[ignore]`. Library code returning on a privilege check
/// is the behaviour under test, not a test declining to run.
fn in_ordinary_test(lines: &[&str], at: usize) -> bool {
    let Some(fn_line) = lines[..at].iter().rposition(|l| {
        let l = l.trim_start();
        l.starts_with("fn ") || l.starts_with("pub fn ") || l.starts_with("pub(crate) fn ")
    }) else {
        return false;
    };
    let attributes: Vec<&str> = lines[..fn_line]
        .iter()
        .rev()
        .map(|l| l.trim())
        .take_while(|l| l.is_empty() || l.starts_with("#[") || l.starts_with("//"))
        .collect();
    attributes.contains(&"#[test]") && !attributes.iter().any(|l| l.starts_with("#[ignore"))
}

#[test]
fn a_test_run_as_root_still_tests_something() {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("tests"), &mut files);
    let offenders = privilege_returns_in_tests(&root, &files);
    assert!(
        offenders.is_empty(),
        "these tests return early on a privilege check, so the privileged tier \
         counts them as passed having tested nothing. Assert what holds for \
         either identity, or shed root in a subprocess:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_privilege_scan_accuses_the_shape_it_bans() {
    // The shape this file exists to forbid, fed to the scan directly, so a
    // scan that silently stopped matching fails here rather than passing
    // everything.
    let dir = tempfile::tempdir().unwrap();
    let sample = dir.path().join("sample.rs");
    std::fs::write(
        &sample,
        "#[test]\nfn t() {\n    if nix::unistd::geteuid().is_root() {\n        return;\n    }\n}\n\n\
         #[test]\n#[ignore]\nfn u() {\n    if !is_root_on_linux() {\n        return;\n    }\n}\n\n\
         fn validate() -> Result<(), E> {\n    if geteuid().as_raw() != 0 {\n        return Err(E);\n    }\n    Ok(())\n}\n",
    )
    .unwrap();
    let offenders = privilege_returns_in_tests(dir.path(), &[sample]);
    assert_eq!(
        offenders,
        ["  sample.rs:3: if nix::unistd::geteuid().is_root() {"],
        "the scan must accuse the ordinary test and exempt the ignored test and \
         the library code"
    );
}
