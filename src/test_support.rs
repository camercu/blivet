//! Shared helpers for tests with process-wide side effects.
//!
//! A test that changes process-wide state — stdio or other descriptors, the
//! umask, cwd, environment, signal handling, credentials — would do so under
//! every test running in parallel in the shared harness. Such a test is marked
//! `#[ignore]` and paired with a wrapper that re-invokes the test binary for
//! just that one test via [`run_in_subprocess`]; the body guards on
//! [`is_subprocess`] so it executes only when spawned this way. The production
//! functions that change that state call [`assert_isolated`] under test, so a
//! test that forgets the subprocess fails instead of racing its neighbours.

use std::ffi::OsStr;
use std::process::{Command, Output};

/// Environment variable set in the spawned subprocess so the `#[ignore]` test
/// body knows it is running in isolation (see [`is_subprocess`]).
const SUBPROCESS_ENV: &str = "__BLIVET_SUBPROCESS_TEST";

/// Set in every child [`rerun_in_subprocess`] starts, whatever marker the
/// caller branches on: each child runs one test, so it alone owns the
/// process-wide state [`assert_isolated`] guards.
const ISOLATED_ENV: &str = "__BLIVET_ISOLATED_TEST";

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
/// here, for the capture described below.
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
///
/// The child is judged on having run the test, not only on how it exited.
/// `test_name` is a literal in the caller, and libtest exits 0 when its filter
/// matches nothing, so a rename or a module move would otherwise turn the
/// caller into a silent no-op that still reports ok — the vacuity
/// `scripts/assert-tests-ran.sh` catches one level up, inside the test binary.
/// A caller that expects the child to fail, or to die on a signal, still gets
/// its own verdict from the returned [`Output`]; this only rules out the child
/// having run nothing.
pub(crate) fn rerun_in_subprocess(
    test_name: &str,
    marker: &str,
    value: impl AsRef<OsStr>,
) -> Output {
    let exe = std::env::current_exe().unwrap();
    let output = Command::new(exe)
        .arg("--exact")
        .arg(test_name)
        .arg("--include-ignored")
        .arg("--nocapture")
        .env(marker, value)
        .env(ISOLATED_ENV, "1")
        .output()
        .unwrap();
    // libtest announces the filtered count before it runs anything, so this
    // holds even for a child that is killed part-way through its body.
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("running 1 test\n"),
        "subprocess test {test_name} matched no test, so its body never ran. \
         Has it been renamed or moved? {}",
        subprocess_report(test_name, &output)
    );
    output
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
/// names that path directly. The bionic tier is where that fails: it runs the
/// library tests on a system with no `/tmp` at all.
/// `std::env::temp_dir()` reads `TMPDIR` and falls back to `/tmp` only where
/// the platform says so, which means a platform without `/tmp` must set
/// `TMPDIR` (the Android CI jobs point it at `/data/local/tmp`).
///
/// Tests whose paths never reach the filesystem do not need this; they name a
/// plainly fictional absolute path such as `/a/x.pid` instead.
pub(crate) fn tmp_dir() -> std::path::PathBuf {
    std::env::temp_dir()
}

/// Fills the descriptor table under a lowered `RLIMIT_NOFILE`, then frees
/// `free` of the descriptors that fill it. Returns the rest; dropping them
/// frees them too.
///
/// Subprocess tests only: the lowered limit stays for the process.
pub(crate) fn fill_fd_table(free: usize) -> Vec<std::os::fd::OwnedFd> {
    use nix::sys::resource::{getrlimit, setrlimit, Resource};
    use std::os::fd::AsFd;

    let (_, hard) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
    setrlimit(Resource::RLIMIT_NOFILE, hard.min(64), hard).unwrap();
    let mut held = Vec::new();
    let full = loop {
        match std::io::stdin().as_fd().try_clone_to_owned() {
            Ok(fd) => held.push(fd),
            Err(e) => break e,
        }
    };
    assert_eq!(
        full.raw_os_error(),
        Some(libc::EMFILE),
        "the fd table did not fill: {full}"
    );
    held.truncate(held.len() - free);
    held
}

/// A pipe for a test to read to EOF: `(read, write)`, close-on-exec like the
/// notification pipe, so a subprocess a parallel `cargo test` thread spawns
/// does not inherit the write end and hold the read open. `RealForker` sets
/// the flag in a second step (macOS has no `pipe2`), so a spawn in between
/// can still inherit it; the read then waits for that subprocess to exit.
pub(crate) fn make_pipe() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
    use crate::forker::{Forker, RealForker};
    RealForker
        .create_notification_pipe()
        .expect("failed to create test pipe")
}

/// Drains a pipe's read end to EOF.
pub(crate) fn read_pipe(rd: std::os::fd::OwnedFd) -> Vec<u8> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::from(rd).read_to_end(&mut buf).unwrap();
    buf
}

/// Panics unless this runs in a child of [`rerun_in_subprocess`].
///
/// For code that changes process-wide state: stdio slots, other descriptors,
/// umask, cwd, environment, signal handling, credentials. The shared test
/// process runs other tests on other threads meanwhile. For fd 0, 1 and 2 the
/// failure was concrete: other threads open descriptors while it runs, and replacing a live stdio slot is not
/// atomic everywhere: NetBSD's `dup2` closes the target, drops the table lock,
/// and closes it again if another thread was handed that number meanwhile. A
/// concurrent `Command` spawn got fd 2 for its status pipe, lost it to the
/// `dup2`, then closed "its" pipe: the harness's stderr. The next saved copy
/// of fd 1 landed on 2, a redirect overwrote it, and the run printed nothing
/// more, not even its summary. A subprocess runs one test, so nothing else
/// opens descriptors under it.
pub(crate) fn assert_isolated(what: &str) {
    assert!(
        std::env::var_os(ISOLATED_ENV).is_some(),
        "{what} changes process-wide state under the shared test process; \
         run the test body through run_in_subprocess"
    );
}
