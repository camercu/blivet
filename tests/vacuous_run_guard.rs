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
    guard_with(inner, None, &[])
}

/// The same, with `tools` ahead of `PATH` so a test can stand in for a program
/// the script depends on, and `options` passed to the guard ahead of the
/// command.
fn guard_with(inner: &str, tools: Option<&std::path::Path>, options: &[&str]) -> Output {
    let script = repo_root().join("scripts/assert-tests-ran.sh");
    assert!(
        script.is_file(),
        "the guard script is missing: {}",
        script.display()
    );
    let mut command = std::process::Command::new("sh");
    command
        .arg(&script)
        .args(options)
        .arg("sh")
        .arg("-c")
        .arg(inner)
        .current_dir(repo_root());
    if let Some(tools) = tools {
        let path = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{}:{path}", tools.display()));
    }
    command.output().expect("the guard script runs")
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

#[test]
fn a_run_whose_counts_could_not_be_read_fails() {
    use std::os::unix::fs::PermissionsExt;

    // GIVEN an `awk` that reports nothing and exits 0, so the count the guard
    // reads back is the empty string rather than a number
    let tools = tempfile::tempdir().unwrap();
    let awk = tools.path().join("awk");
    std::fs::write(&awk, "exit 0\n").unwrap();
    std::fs::set_permissions(&awk, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = guard_with(
        "echo 'test result: ok. 0 passed; 0 failed'",
        Some(tools.path()),
        &[],
    );

    // THEN the guard fails closed. A run of zero tests reaching `exit 0`
    // because the count was unreadable is the outcome the guard exists to
    // prevent, arrived at from the other side.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a count the guard could not read must not pass; got:\n{combined}"
    );
    assert!(
        combined.contains("counts"),
        "the guard must say it could not read the counts; got:\n{combined}"
    );
}

/// A nextest summary line, as nextest prints it. `colour` wraps the parts
/// nextest highlights, which CI turns on through `CARGO_TERM_COLOR`.
fn nextest_summary(count: &str, word: &str, colour: bool) -> String {
    let (on, off) = if colour {
        ("\\033[1m", "\\033[0m")
    } else {
        ("", "")
    };
    format!(
        "printf '   {on}Summary{off} [   0.007s] {count} {word} run: {count} passed, 3 skipped\\n'"
    )
}

#[test]
fn a_nextest_run_that_ran_tests_passes() {
    for colour in [false, true] {
        // GIVEN a healthy nextest run, which reports its counts in nextest's
        // words rather than libtest's
        let out = guard(&nextest_summary("271", "tests", colour));

        // THEN the guard accepts it. Reporting a run of 271 tests as one that
        // proved nothing is the wrong diagnosis from a script whose whole
        // product is the right diagnosis.
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "a nextest run of 271 tests must pass (colour: {colour}); got:\n{combined}"
        );
    }
}

#[test]
fn a_nextest_run_that_ran_nothing_fails() {
    for colour in [false, true] {
        // GIVEN a nextest run whose filter matched nothing, which nextest also
        // reports as success
        let out = guard(&nextest_summary("0", "tests", colour));

        // THEN the guard rejects it, exactly as it does the libtest form
        assert!(
            !out.status.success(),
            "a nextest run of zero tests must not pass (colour: {colour})"
        );
    }
}

#[test]
fn a_run_asked_to_leave_nothing_out_rejects_a_skip() {
    // GIVEN a caller that asked for every test, and a run that skipped some
    let out = guard_with(
        &nextest_summary_with_skips("271", "33"),
        None,
        &["--no-skips"],
    );

    // THEN the guard rejects it. "Tests ran" is not the claim here; "these
    // tests ran" is, and a skip is coverage the caller says it has.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a run that skipped 33 tests must not pass; got:\n{combined}"
    );
    assert!(
        combined.contains("33 test(s) were skipped"),
        "the guard must name the skip count; got:\n{combined}"
    );
}

#[test]
fn a_run_that_left_nothing_out_passes() {
    // GIVEN the same demand and a run that skipped nothing
    let out = guard_with(
        &nextest_summary_with_skips("271", "0"),
        None,
        &["--no-skips"],
    );

    // THEN the guard accepts it
    assert!(
        out.status.success(),
        "a run that skipped nothing must pass; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_skip_is_ignored_unless_the_caller_asked() {
    // GIVEN a run that skipped tests, from a caller that did not demand none
    let out = guard_with(&nextest_summary_with_skips("271", "33"), None, &[]);

    // THEN the guard accepts it: most tiers legitimately filter, and only the
    // caller knows whether its own claim covers what it left out.
    assert!(
        out.status.success(),
        "a skip must not fail a caller that did not ask; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A nextest summary reporting `count` tests run and `skipped` left out.
fn nextest_summary_with_skips(count: &str, skipped: &str) -> String {
    format!(
        "printf '   Summary [   0.007s] {count} tests run: {count} passed, {skipped} skipped\\n'"
    )
}
