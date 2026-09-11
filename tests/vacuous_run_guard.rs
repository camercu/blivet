//! A container tier must not report success having run no tests.
//!
//! `cargo test` prints `test result: ok.` and exits 0 when it matches no
//! tests, so a container whose sources look stale to cargo — the warm
//! dependency layer leaving a fingerprint newer than the source `COPY` that
//! follows it — goes green having proven nothing. `Dockerfile.termux` avoids
//! that staleness by hand; these tests cover the backstop that notices when
//! some future container does not.

use std::path::PathBuf;
use std::process::Output;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Runs the guard over `inner`, a shell command standing in for a test run.
///
/// Invoked as `sh <script>` rather than executed directly: Termux has no
/// `/bin/sh` for a shebang to name, so the containers call it this way too.
fn guard(inner: &str) -> Output {
    let script = repo_root().join("scripts/assert-tests-ran.sh");
    assert!(
        script.is_file(),
        "the guard script is missing: {}",
        script.display()
    );
    std::process::Command::new("sh")
        .arg(&script)
        .arg("sh")
        .arg("-c")
        .arg(inner)
        .current_dir(repo_root())
        .output()
        .expect("the guard script runs")
}

#[test]
fn a_run_that_reported_no_tests_fails() {
    // GIVEN a test run that matched nothing, which cargo reports as success
    let out = guard("echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured'");

    // THEN the guard rejects it
    assert!(
        !out.status.success(),
        "a run of zero tests must not pass; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_run_that_reported_tests_passes() {
    // GIVEN a test run that actually ran tests
    let out = guard("echo 'test result: ok. 197 passed; 0 failed; 14 ignored; 0 measured'");

    // THEN the guard accepts it
    assert!(
        out.status.success(),
        "a run of 197 tests must pass; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_failing_run_stays_failed() {
    // GIVEN a run that ran tests and failed some
    let out = guard("echo 'test result: FAILED. 3 passed; 2 failed; 0 ignored'; exit 101");

    // THEN the guard forwards the failure rather than masking it behind its
    // own "tests did run" verdict
    assert_eq!(
        out.status.code(),
        Some(101),
        "the test command's exit status must survive the guard; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_run_where_everything_failed_is_not_reported_as_no_tests() {
    // GIVEN a run that executed tests and every one of them failed
    let out = guard("echo 'test result: FAILED. 0 passed; 5 failed; 0 ignored'; exit 101");

    // THEN the guard forwards that failure rather than claiming nothing ran:
    // the counts prove tests did run, and naming the wrong cause sends the
    // reader looking for a stale build that is not there.
    assert_eq!(
        out.status.code(),
        Some(101),
        "an all-failed run must keep its own status"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("reported no tests at all"),
        "5 tests ran and failed, so the no-tests diagnosis is wrong; got:\n{combined}"
    );
}

#[test]
fn a_command_that_failed_before_running_tests_keeps_its_own_error() {
    // GIVEN a command that failed before any test could run — a compile error
    // is the usual one — so it printed no summary line at all
    let out = guard("echo 'error[E0308]: mismatched types' >&2; exit 101");

    // THEN the guard forwards that failure. A build that never got as far as
    // running tests is not the stale-source case, and saying so sends the
    // reader hunting a problem that is not there.
    assert_eq!(
        out.status.code(),
        Some(101),
        "a command that failed outright must keep its status"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("reported no tests at all"),
        "the command failed before running tests; got:\n{combined}"
    );
}

#[test]
fn a_command_killed_before_it_reported_a_status_says_so() {
    // GIVEN the guard's own subshell dies before it can record the exit status
    let out = guard("kill -9 $PPID");

    // THEN the guard names that, rather than letting a failed `cat` speak for
    // it. A script whose whole job is the right diagnosis must not answer with
    // a missing-file error about its own scratch directory.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a run that never reported a status must not pass"
    );
    assert!(
        !combined.contains("cat:"),
        "the guard must diagnose the killed run itself; got:\n{combined}"
    );
    assert!(
        combined.contains("killed"),
        "the guard must say the command was killed; got:\n{combined}"
    );
}

/// The guard script, as every tier names it.
const GUARD: &str = "assert-tests-ran.sh";

/// Every file that invokes the guard, and so is subject to the rule below.
const CALLERS: [&str; 5] = [
    "Dockerfile",
    "Dockerfile.termux",
    "scripts/android-smoke.sh",
    ".github/workflows/ci.yml",
    "justfile",
];

/// The commands each guard invocation in `text` wraps.
///
/// An invocation ends at the end of its logical line — a trailing `\` joins the
/// next one — or where the next invocation begins, since a tier may chain two
/// guarded commands together.
fn wrapped_commands(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices(GUARD) {
        let rest = &text[at + GUARD.len()..];
        let end = rest.find(GUARD).unwrap_or(rest.len());
        let mut command = String::new();
        for line in rest[..end].lines() {
            command.push_str(line);
            if !line.trim_end().ends_with('\\') {
                break;
            }
        }
        out.push(command);
    }
    out
}

#[test]
fn a_guard_invocation_wraps_one_test_command() {
    // The guard sums counts across every suite the wrapped command runs, which
    // its own header states. Chaining a second test command into the same
    // invocation therefore lets that command's counts cover the first's zero —
    // and `cargo test --doc` can never report zero, because rustdoc re-reads
    // the sources every run and has no stale binary to reuse. A tier that
    // chains the two guards its own suite for nothing.
    //
    // Counted on `cargo test`, so a tier running a prebuilt test binary — the
    // NetBSD VM and the Android device — is outside what this proves; each of
    // those runs exactly one binary today.
    let root = repo_root();
    let mut offenders = Vec::new();
    for caller in CALLERS {
        let path = root.join(caller);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{caller} must be readable: {e}"));
        for command in wrapped_commands(&text) {
            let runs = command.matches("cargo test").count();
            if runs > 1 {
                offenders.push(format!(
                    "  {caller}: {runs} test commands in one guard invocation:\n    {}",
                    command.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a guard invocation must wrap one test command:\n{}\n\
         Give each command its own `sh scripts/{GUARD}` prefix, so neither \
         command's counts can stand in for the other's.",
        offenders.join("\n")
    );
}
