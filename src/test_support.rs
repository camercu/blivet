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

use std::process::Command;

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
