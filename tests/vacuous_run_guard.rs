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
