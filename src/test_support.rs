//! Shared helpers for tests that must run in an isolated subprocess.
//!
//! Some tests have process-wide side effects — redirecting std fds, closing
//! inherited fds, forking — that corrupt the shared test harness or clobber
//! file descriptors held by tests running in parallel. Such a test is marked
//! `#[ignore]` (so it never runs in the normal pass) and paired with a wrapper
//! that re-invokes the test binary for just that one test via
//! [`run_in_subprocess`]. The body guards on [`is_subprocess`] so it executes
//! only when spawned this way.

use std::process::Command;

/// Environment variable set in the spawned subprocess so the `#[ignore]` test
/// body knows it is running in isolation (see [`is_subprocess`]).
const SUBPROCESS_ENV: &str = "__BLIVET_SUBPROCESS_TEST";

/// Returns `true` when running inside a subprocess spawned by
/// [`run_in_subprocess`].
pub(crate) fn is_subprocess() -> bool {
    std::env::var(SUBPROCESS_ENV).is_ok()
}

/// Re-invokes the test binary to run a single `#[ignore]` test in its own
/// process, then asserts it succeeded.
///
/// `--include-ignored` is required: without it the named `#[ignore]` test is
/// skipped, the subprocess exits 0, and this helper passes *vacuously* without
/// ever running the test body.
pub(crate) fn run_in_subprocess(test_name: &str) {
    let exe = std::env::current_exe().unwrap();
    let status = Command::new(exe)
        .arg("--exact")
        .arg(test_name)
        .arg("--include-ignored") // the target test is #[ignore]
        .arg("--nocapture")
        .env(SUBPROCESS_ENV, "1")
        .status()
        .unwrap();
    assert!(status.success(), "subprocess test failed: {status}");
}
