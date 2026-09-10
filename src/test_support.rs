//! Shared helpers for tests with process-wide side effects.
//!
//! Some tests touch process-global state — redirecting std fds, closing
//! inherited fds, forking, changing the umask — that corrupts the shared test
//! harness or clobbers state used by tests running in parallel. Two tools
//! contain the damage:
//!
//! - A test whose effects cannot be undone in-process (fd closing, forking) is
//!   marked `#[ignore]` and paired with a wrapper that re-invokes the test
//!   binary for just that one test via [`run_in_subprocess`]. The body guards
//!   on [`is_subprocess`] so it executes only when spawned this way.
//! - Reversible global state (the umask) gets an RAII guard ([`UmaskGuard`])
//!   so a panicking assertion cannot leak the altered state.

use std::ffi::OsStr;
use std::process::{Command, Output};

/// RAII guard: sets the process umask and restores the previous one on drop.
///
/// Restoring in a `Drop` (rather than a trailing statement) means a panicking
/// assertion between set and restore cannot leak the altered umask into tests
/// running in parallel. Pair with `#[serial]` on the test so concurrent umask
/// users are excluded too.
pub(crate) struct UmaskGuard {
    old: nix::sys::stat::Mode,
}

impl UmaskGuard {
    pub(crate) fn set(mode: nix::sys::stat::Mode) -> Self {
        Self {
            old: nix::sys::stat::umask(mode),
        }
    }
}

impl Drop for UmaskGuard {
    fn drop(&mut self) {
        nix::sys::stat::umask(self.old);
    }
}

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
    let output = rerun_in_subprocess(test_name, SUBPROCESS_ENV, "1");
    assert!(
        output.status.success(),
        "{}",
        subprocess_report(test_name, &output)
    );
}

/// Re-invokes this test binary to run one test in its own process, with
/// `marker` set to `value`, and returns what the child exited with.
///
/// The child's body branches on `marker`, so the same test source is both the
/// caller and the isolated run. Every re-invocation in the crate goes through
/// here; `tests/self_reinvocation.rs` enforces that.
///
/// The child's output is captured, not inherited. A child writing straight to
/// the parent's stdout interleaves with it, and with the other children running
/// alongside, which can split a line so that whatever reads the run's summary
/// lines no longer matches one; the child also prints a summary line of its
/// own, which `scripts/assert-tests-ran.sh` then counts. A caller reports the
/// captured streams only when the child failed, which keeps the diagnosis
/// without the corruption — see [`subprocess_report`].
///
/// `--include-ignored` is required: without it a test marked `#[ignore]` is
/// skipped, the child exits 0, and the caller passes *vacuously* without ever
/// running the body.
pub(crate) fn rerun_in_subprocess(
    test_name: &str,
    marker: &str,
    value: impl AsRef<OsStr>,
) -> Output {
    let exe = std::env::current_exe().unwrap();
    Command::new(exe)
        .arg("--exact")
        .arg(test_name)
        .arg("--include-ignored")
        .arg("--nocapture")
        .env(marker, value)
        .output()
        .unwrap()
}

/// Renders a finished subprocess for a failure message: how it ended, and both
/// of its captured streams.
pub(crate) fn subprocess_report(test_name: &str, output: &Output) -> String {
    format!(
        "subprocess test {test_name} failed: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

/// Directory for test paths that reach the filesystem, honouring `TMPDIR`.
///
/// `/tmp` is not universal — Android and Termux ship without it — so no test
/// names that path directly; `tests/location_independence.rs` enforces that.
/// `std::env::temp_dir()` reads `TMPDIR` and falls back to `/tmp` only where
/// the platform says so, which means a platform without `/tmp` must set
/// `TMPDIR` (the Android CI jobs point it at `/data/local/tmp`).
///
/// Tests whose paths never reach the filesystem do not need this; they name a
/// plainly fictional absolute path such as `/a/x.pid` instead.
pub(crate) fn tmp_dir() -> std::path::PathBuf {
    std::env::temp_dir()
}
