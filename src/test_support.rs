//! Shared helpers for tests with process-wide side effects.
//!
//! A test that changes process-wide state — stdio or other descriptors, the
//! umask, cwd, environment, signal handling, credentials — would do so under
//! every test running in parallel in the shared harness. Such a test opens with
//! `if !isolated() { return; }`: [`isolated`] re-invokes the test binary for
//! just that one test and returns `true` only in that child. The production
//! functions that change that state call [`assert_isolated`] under test, so a
//! test that forgets the guard fails instead of racing its neighbours.

use std::ffi::OsStr;
use std::process::{Command, Output};

/// Environment variable set in the child [`isolated`] starts, to the name of
/// the one test the child runs (see [`is_child_for`]).
const SUBPROCESS_ENV: &str = "__BLIVET_SUBPROCESS_TEST";

/// Set in every child [`rerun_in_subprocess`] starts, whatever marker the
/// caller branches on: each child runs one test, so it alone owns the
/// process-wide state [`assert_isolated`] guards.
const ISOLATED_ENV: &str = "__BLIVET_ISOLATED_TEST";

/// Whether test `test_name` runs in the child [`isolated`] started for it,
/// given the [`SUBPROCESS_ENV`] value `marker`. A marker that names another
/// test does not count: a value leaked into a whole-suite run must not let
/// every isolated body run in the shared harness.
fn is_child_for(test_name: &str, marker: Option<&OsStr>) -> bool {
    marker == Some(OsStr::new(test_name))
}

/// Runs the calling test again in its own process. Returns `true` in that
/// child, where the test body runs, and `false` in the parent once the child
/// passed. Open each isolated test with `if !isolated() { return; }`.
///
/// The test's name comes from libtest, which names each test's thread after
/// the test, so no test spells its own name. A thread with no name means the
/// caller is not a test, and this panics. So does `main`: libtest falls back
/// to the main thread when it cannot spawn one, and that name names no test.
pub(crate) fn isolated() -> bool {
    let thread = std::thread::current();
    let test_name = thread.name().filter(|name| *name != "main").expect(
        "isolated() runs only on a libtest thread named after its test; \
         this thread is unnamed or `main`",
    );
    if is_child_for(test_name, std::env::var_os(SUBPROCESS_ENV).as_deref()) {
        return true;
    }
    let output = rerun_in_subprocess(test_name, SUBPROCESS_ENV, test_name);
    assert!(
        output.status.success(),
        "{}",
        subprocess_report(test_name, &output)
    );
    false
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
/// A caller that spells `test_name` as a literal goes stale on a rename or a
/// module move, and libtest exits 0 when its filter matches nothing or skips
/// an ignored test, so the caller would otherwise turn into a silent no-op
/// that still reports ok. `scripts/assert-tests-ran.sh` catches the same
/// vacuity one level up, inside the test binary.
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
    assert!(
        child_ran(test_name, &String::from_utf8_lossy(&output.stdout)),
        "subprocess test {test_name} matched no test or was skipped as ignored, \
         so its body never ran. Has it been renamed or moved? {}",
        subprocess_report(test_name, &output)
    );
    output
}

/// Whether a child's stdout shows libtest started `test_name`, rather than
/// matching nothing or skipping it as ignored. libtest announces the count
/// before it runs anything, so this holds for a child killed mid-body too.
fn child_ran(test_name: &str, stdout: &str) -> bool {
    stdout.contains("running 1 test\n")
        && !stdout.contains(&format!("test {test_name} ... ignored"))
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
/// The lowered limit stays for the process, so this asserts it runs in its own.
pub(crate) fn fill_fd_table(free: usize) -> Vec<std::os::fd::OwnedFd> {
    assert_isolated("fill_fd_table");
    use nix::sys::resource::{Resource, getrlimit, setrlimit};
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

/// Runs `f` with the descriptors in `closed` closed, then puts each back.
///
/// For tests of what lands in a freed stdio slot. Closing stdio changes the
/// process, so this asserts it runs in its own.
#[allow(unsafe_code)]
pub(crate) fn with_fds_closed<R>(closed: &[i32], f: impl FnOnce() -> R) -> R {
    assert_isolated("with_fds_closed");
    let saved: Vec<std::os::fd::OwnedFd> = closed
        .iter()
        .map(|&fd| {
            // SAFETY: `fd` is a stdio slot this test process holds open.
            let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            fd.try_clone_to_owned().expect("dup")
        })
        .collect();
    for &fd in closed {
        crate::sys::raw_close(fd);
    }
    let result = f();
    for (&fd, copy) in closed.iter().zip(&saved) {
        #[allow(clippy::disallowed_methods)] // puts back the slots it closed
        // SAFETY: `dup2` onto a closed slot touches no memory, and no handle
        // owns the slot.
        let put_back = unsafe { libc::dup2(std::os::fd::AsRawFd::as_raw_fd(copy), fd) };
        assert_eq!(put_back, fd, "could not put back fd {fd}");
    }
    result
}

/// A pipe for a test to read to EOF: `(read, write)`, close-on-exec like the
/// notification channel, so a subprocess a parallel `cargo test` thread spawns
/// does not inherit the write end and hold the read open. On macOS the flag
/// is set in a second step (no `SOCK_CLOEXEC`), so a spawn in between can
/// still inherit it; the read then waits for that subprocess to exit.
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
/// failure was concrete: other threads open descriptors while it runs, and
/// replacing a live stdio slot is not atomic everywhere: NetBSD's `dup2` closes
/// the target, drops the table lock, and closes it again if another thread was
/// handed that number meanwhile. A concurrent `Command` spawn got fd 2 for its
/// status pipe, lost it to the `dup2`, then closed "its" pipe: the harness's
/// stderr. The next saved copy of fd 1 landed on 2, a redirect overwrote it,
/// and the run printed nothing more, not even its summary. A subprocess runs
/// one test, so nothing else opens descriptors under it.
pub(crate) fn assert_isolated(what: &str) {
    assert!(
        std::env::var_os(ISOLATED_ENV).is_some(),
        "{what} changes process-wide state under the shared test process; \
         open the test with `if !isolated() {{ return; }}`"
    );
}

/// Linux's `SA_RESTORER`. On x86_64, glibc and bionic add it to every
/// action they install, so a restored action differs from the original
/// in this bit alone. It names libc's return trampoline, not behaviour.
const SA_RESTORER: i32 = 0x0400_0000;

/// One signal's action: handler, flags less `SA_RESTORER`, and the signals
/// its handler blocks.
pub(crate) type SignalAction = (i32, usize, i32, Vec<i32>);

/// Every signal's action, and the signals the thread blocks, real-time ones
/// included.
#[allow(unsafe_code)]
pub(crate) fn signal_state() -> (Vec<SignalAction>, Vec<i32>) {
    // The members of `set` among 1..=64; `sigismember` rejects the rest.
    let members = |set: &libc::sigset_t| -> Vec<i32> {
        // SAFETY: `set` is an initialised signal set.
        (1..=64)
            .filter(|&sig| unsafe { libc::sigismember(set, sig) } == 1)
            .collect()
    };
    let mut dispositions = Vec::new();
    for sig in 1..=64 {
        if let Ok(now) = crate::signals::disposition(sig) {
            let flags = now.sa_flags & !SA_RESTORER;
            dispositions.push((sig, now.sa_sigaction, flags, members(&now.sa_mask)));
        }
    }
    let mut mask = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: a null new set only reads the thread's mask into `mask`.
    let ret =
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), mask.as_mut_ptr()) };
    assert_eq!(ret, 0, "pthread_sigmask");
    // SAFETY: `pthread_sigmask` succeeded, so it filled `mask`.
    let blocked = members(unsafe { mask.assume_init_ref() });
    (dispositions, blocked)
}

/// The process state a failed foreground start must leave as it found it
/// (R164), as the undo walk compares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessSnapshot {
    umask: nix::sys::stat::Mode,
    cwd: std::path::PathBuf,
    /// What fds 0 to 2 point at, by device and inode; `None` when closed.
    stdio: [Option<crate::file_identity::FileId>; 3],
    signals: (Vec<SignalAction>, Vec<i32>),
    /// Sorted; `None` once compared without it.
    env: Option<Vec<(std::ffi::OsString, std::ffi::OsString)>>,
    groups: Vec<libc::gid_t>,
    gid: u32,
    egid: u32,
    pidfile_exists: bool,
}

impl ProcessSnapshot {
    /// Reads the state now. `pidfile` is the file the start writes.
    pub(crate) fn take(pidfile: &std::path::Path) -> Self {
        use nix::sys::stat::{Mode, umask};
        let mask = umask(Mode::empty());
        umask(mask);
        let stdio = [0, 1, 2].map(|fd| {
            // SAFETY: `fstat` only reads the slot; a closed one is EBADF.
            #[allow(unsafe_code)]
            let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            crate::file_identity::file_id(fd).ok()
        });
        let mut env: Vec<_> = std::env::vars_os().collect();
        env.sort();
        let mut groups = supplementary_gids();
        groups.sort_unstable();
        Self {
            umask: mask,
            cwd: std::env::current_dir().unwrap(),
            stdio,
            signals: signal_state(),
            env: Some(env),
            groups,
            gid: nix::unistd::getgid().as_raw(),
            egid: nix::unistd::getegid().as_raw(),
            pidfile_exists: pidfile.exists(),
        }
    }

    /// This snapshot without the environment, which a failure after `init`
    /// leaves as step 11 set it (R164).
    pub(crate) fn without_env(mut self) -> Self {
        self.env = None;
        self
    }
}

/// This process's supplementary group IDs.
pub(crate) fn supplementary_gids() -> Vec<libc::gid_t> {
    crate::sys::raw_getgroups().expect("getgroups failed")
}

/// A gid the current user cannot chown files to: not the effective gid and
/// not a supplementary group. An unprivileged chown to it fails `EPERM`, so a
/// test sees that the chown was attempted, and it fails before any `setgid`
/// changes the test process's groups.
pub(crate) fn foreign_gid() -> u32 {
    let gids = supplementary_gids();
    let egid = nix::unistd::getegid().as_raw();
    (1..).find(|g| !gids.contains(g) && *g != egid).unwrap()
}

/// Drops the effective UID to `nobody` when the test runs as root.
///
/// The tests that call this assert an unprivileged failure, a chown or a
/// `setgid` refused with `EPERM`. Root would succeed instead, so rather than
/// count a pass the root tier never tested, the body sheds root in its own
/// process. Process-global, so it asserts it runs in one.
pub(crate) fn shed_root() {
    assert_isolated("shed_root");
    /// The conventional unprivileged UID; `seteuid` does not need it in passwd.
    const NOBODY: u32 = 65534;
    if nix::unistd::geteuid().is_root() {
        nix::unistd::seteuid(nix::unistd::Uid::from_raw(NOBODY))
            .expect("root can drop its effective UID");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_runs_the_body_only_for_the_test_its_marker_names() {
        let me = "steps::process::tests::x";
        assert!(is_child_for(me, Some(me.as_ref())));
        assert!(!is_child_for(me, Some("steps::process::tests::y".as_ref())));
        assert!(!is_child_for(me, None));
    }

    /// A thread named `main` names no test, so rerunning it would match nothing.
    #[test]
    #[should_panic(expected = "this thread is unnamed or `main`")]
    fn isolated_refuses_the_main_thread() {
        let main = std::thread::Builder::new().name("main".to_owned());
        let joined = main.spawn(isolated).unwrap().join();
        std::panic::resume_unwind(joined.unwrap_err());
    }

    #[test]
    fn a_child_skipped_as_ignored_did_not_run() {
        let me = "sequence::tests::x";
        assert!(child_ran(
            me,
            "\nrunning 1 test\ntest sequence::tests::x ... ok\n"
        ));
        assert!(!child_ran(
            me,
            "\nrunning 1 test\ntest sequence::tests::x ... ignored, needs root\n"
        ));
        assert!(!child_ran(me, "\nrunning 0 tests\n"));
    }

    /// The parent fails when its child does: otherwise every isolated test
    /// passes whatever its body asserts. The child exits rather than panics,
    /// so its own libtest verdict plays no part.
    #[test]
    fn a_failing_isolated_body_fails_the_parent() {
        match std::panic::catch_unwind(isolated) {
            Ok(true) => std::process::exit(3),
            Ok(false) => panic!("the parent passed a child that failed"),
            Err(panic) => {
                let msg = panic.downcast::<String>().unwrap();
                assert!(msg.contains("exit status: 3"), "{msg}");
            }
        }
    }

    #[test]
    fn an_isolated_test_runs_its_body_in_a_child_process() {
        let ran = |pid: u32| tmp_dir().join(format!("blivet-isolated-ran-{pid}"));
        if std::env::var_os(SUBPROCESS_ENV).is_none() {
            // A marker left by a crashed run under a reused pid would pass.
            let _ = std::fs::remove_file(ran(std::process::id()));
        }
        if !isolated() {
            let marker = ran(std::process::id());
            let body_ran = marker.exists();
            let _ = std::fs::remove_file(&marker);
            assert!(body_ran, "the isolated body never ran");
            return;
        }
        assert_ne!(
            std::env::var_os(ISOLATED_ENV),
            None,
            "the body ran outside a child"
        );
        let parent = std::os::unix::process::parent_id();
        std::fs::write(ran(parent), b"").unwrap();
    }
}
