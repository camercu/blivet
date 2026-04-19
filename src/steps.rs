//! Post-fork daemonization steps.
//!
//! Each function corresponds to one step in the daemonization sequence
//! orchestrated by [`daemonize_inner`](crate::daemonize_inner). They are
//! collected here rather than inlined because each is independently testable.

use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use nix::fcntl::{open, Flock, FlockArg, OFlag};
use nix::sys::stat::Mode;
use nix::unistd::Whence;

use crate::error::DaemonizeError;
use crate::unsafe_ops;
use crate::util::paths_same;

// ---- Plan/Execute types for redirect_output ----

/// Describes what to do with a single output stream (stdout or stderr).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamAction {
    /// Leave the stream as-is (already redirected to /dev/null).
    None,
    /// Open a file and redirect the target fd to it.
    OpenAndRedirect {
        path: PathBuf,
        flags: OFlag,
        target_fd: i32,
    },
    /// Dup an already-open fd to the target fd.
    DupFrom { source_fd: i32, target_fd: i32 },
}

/// Plan for redirecting stdout and stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputRedirectPlan {
    pub(crate) stdout: StreamAction,
    pub(crate) stderr: StreamAction,
}

/// Step 4: Set process umask.
pub(crate) fn set_umask(mode: Mode) {
    nix::sys::stat::umask(mode);
}

/// Step 5: Change working directory.
pub(crate) fn change_dir(path: &Path) -> Result<(), DaemonizeError> {
    nix::unistd::chdir(path)
        .map_err(|e| DaemonizeError::ChdirFailed(format!("{}: {e}", path.display())))
}

/// Step 6: Redirect stdin, stdout, stderr to /dev/null.
///
/// # Panics
///
/// Panics if /dev/null cannot be opened or dup2 fails.
pub(crate) fn redirect_to_devnull() {
    let devnull_path = c"/dev/null";
    let devnull_raw =
        unsafe_ops::raw_open(devnull_path, libc::O_RDWR, 0).expect("failed to open /dev/null");

    for &target_fd in &[0, 1, 2] {
        if devnull_raw != target_fd {
            unsafe_ops::raw_dup2(devnull_raw, target_fd).expect("failed to dup2 /dev/null");
        }
    }
    if devnull_raw > 2 {
        unsafe_ops::raw_close(devnull_raw);
    }
}

/// Step 7: Open and exclusively lock a lockfile.
pub(crate) fn open_and_lock(path: &Path) -> Result<Flock<OwnedFd>, DaemonizeError> {
    let fd = open(
        path,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o644),
    )
    .map_err(|e| DaemonizeError::LockfileError(format!("cannot open {}: {e}", path.display())))?;

    Flock::lock(fd, FlockArg::LockExclusiveNonblock).map_err(|(_fd, e)| {
        if e == nix::errno::Errno::EWOULDBLOCK {
            DaemonizeError::LockConflict(format!(
                "{} is already locked by another process",
                path.display()
            ))
        } else {
            DaemonizeError::LockfileError(format!("flock {}: {e}", path.display()))
        }
    })
}

/// Step 8: Write PID to pidfile.
///
/// If the pidfile is the same path as the lockfile, seeks to 0, truncates,
/// and writes to the already-locked fd. Otherwise opens, writes, and closes.
pub(crate) fn write_pidfile(
    pidfile_path: &Path,
    lockfile_path: Option<&PathBuf>,
    lockfile: Option<&Flock<OwnedFd>>,
) -> Result<(), DaemonizeError> {
    let pid = std::process::id();
    let content = format!("{pid}\n");

    // Check if pidfile is the same as lockfile
    let shared = match (lockfile_path, lockfile) {
        (Some(lp), Some(flock)) if paths_same(pidfile_path, lp) => Some(flock),
        _ => None,
    };

    if let Some(flock) = shared {
        // Write to already-locked fd: seek, truncate, write
        nix::unistd::lseek(flock.as_fd(), 0, Whence::SeekSet)
            .map_err(|e| DaemonizeError::PidfileError(format!("seek: {e}")))?;
        nix::unistd::ftruncate(flock.as_fd(), 0)
            .map_err(|e| DaemonizeError::PidfileError(format!("truncate: {e}")))?;
        write_all_fd(flock.as_fd(), content.as_bytes())
            .map_err(|e| DaemonizeError::PidfileError(format!("write: {e}")))?;
    } else {
        // Open, write, close
        std::fs::write(pidfile_path, content.as_bytes()).map_err(|e| {
            DaemonizeError::PidfileError(format!("write {}: {e}", pidfile_path.display()))
        })?;
    }

    Ok(())
}

/// Step 10: Clear the signal mask.
///
/// # Panics
///
/// Panics if sigprocmask fails.
pub(crate) fn clear_signal_mask() {
    use nix::sys::signal::{SigSet, SigmaskHow};
    nix::sys::signal::sigprocmask(SigmaskHow::SIG_SETMASK, Some(&SigSet::empty()), None)
        .expect("sigprocmask failed");
}

/// Step 11: Set environment variables in insertion order.
///
/// # Safety context
///
/// `std::env::set_var` is not thread-safe. This is called post-fork in a
/// single-threaded child process, so no data race is possible.
#[allow(unsafe_code)]
pub(crate) fn set_env_vars(env: &[(String, String)]) {
    for (key, value) in env {
        // SAFETY: post-fork, single-threaded — no concurrent readers.
        unsafe { std::env::set_var(key, value) };
    }
}

/// Build an [`OutputRedirectPlan`] describing how to redirect stdout/stderr.
///
/// This is pure logic with no side effects — it decides *what* to do based on
/// the configured paths and append flag, without touching any file descriptors.
pub(crate) fn plan_output_redirect(
    stdout: Option<&PathBuf>,
    stderr: Option<&PathBuf>,
    append: bool,
) -> OutputRedirectPlan {
    let mut flags = OFlag::O_WRONLY | OFlag::O_CREAT;
    if append {
        flags |= OFlag::O_APPEND;
    } else {
        flags |= OFlag::O_TRUNC;
    }

    let same_path = match (stdout, stderr) {
        (Some(out), Some(err)) => paths_same(out, err),
        _ => false,
    };

    let stdout_action = match stdout {
        Some(path) => StreamAction::OpenAndRedirect {
            path: path.clone(),
            flags,
            target_fd: 1,
        },
        None => StreamAction::None,
    };

    let stderr_action = if same_path {
        StreamAction::DupFrom {
            source_fd: 1,
            target_fd: 2,
        }
    } else {
        match stderr {
            Some(path) => StreamAction::OpenAndRedirect {
                path: path.clone(),
                flags,
                target_fd: 2,
            },
            None => StreamAction::None,
        }
    };

    OutputRedirectPlan {
        stdout: stdout_action,
        stderr: stderr_action,
    }
}

/// Execute a single [`StreamAction`], performing the actual fd operations.
fn execute_stream_action(action: &StreamAction) -> Result<(), DaemonizeError> {
    match action {
        StreamAction::None => Ok(()),
        StreamAction::OpenAndRedirect {
            path,
            flags,
            target_fd,
        } => {
            let mode = Mode::from_bits_truncate(0o644);
            let fd = open(path, *flags, mode).map_err(|e| {
                DaemonizeError::OutputFileError(format!("cannot open {}: {e}", path.display()))
            })?;
            let raw = fd.as_raw_fd();
            if raw != *target_fd {
                unsafe_ops::raw_dup2(raw, *target_fd).map_err(|e| {
                    DaemonizeError::OutputFileError(format!("dup2 fd {target_fd}: {e}"))
                })?;
                unsafe_ops::raw_close(raw);
            }
            // Prevent OwnedFd destructor from closing the fd — ownership is
            // transferred to the target fd slot via dup2, or the fd already
            // was the target and must stay open.
            std::mem::forget(fd);
            Ok(())
        }
        StreamAction::DupFrom {
            source_fd,
            target_fd,
        } => {
            unsafe_ops::raw_dup2(*source_fd, *target_fd).map_err(|e| {
                DaemonizeError::OutputFileError(format!("dup2 fd {source_fd} -> {target_fd}: {e}"))
            })?;
            Ok(())
        }
    }
}

/// Execute an [`OutputRedirectPlan`], performing all fd operations.
pub(crate) fn execute_output_redirect(plan: &OutputRedirectPlan) -> Result<(), DaemonizeError> {
    execute_stream_action(&plan.stdout)?;
    execute_stream_action(&plan.stderr)?;
    Ok(())
}

/// Step 12: Redirect stdout/stderr to configured files.
///
/// Opened after user switching so files have correct ownership.
/// Same-path optimization: if stdout and stderr resolve to the same path,
/// open once for stdout and dup2 to stderr.
pub(crate) fn redirect_output(
    stdout: Option<&PathBuf>,
    stderr: Option<&PathBuf>,
    append: bool,
) -> Result<(), DaemonizeError> {
    let plan = plan_output_redirect(stdout, stderr, append);
    execute_output_redirect(&plan)
}

/// Write all bytes to a file descriptor, looping on partial writes.
fn write_all_fd(fd: impl AsFd, buf: &[u8]) -> Result<(), nix::errno::Errno> {
    let fd = fd.as_fd();
    let mut written = 0;
    while written < buf.len() {
        match nix::unistd::write(fd, &buf[written..]) {
            Ok(0) => return Err(nix::errno::Errno::EIO),
            Ok(n) => written += n,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Query the process fd limit via getrlimit.
///
/// # Panics
///
/// Panics if getrlimit fails.
pub(crate) fn get_max_fd() -> i32 {
    let limit = nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE)
        .expect("getrlimit(RLIMIT_NOFILE) failed");
    limit.0 as i32
}

/// Return an iterator of fd numbers to close, filtering out skip_fds.
///
/// Pure logic — no side effects.
pub(crate) fn fds_to_close(max_fd: i32, skip_fds: &[i32]) -> impl Iterator<Item = i32> + '_ {
    (3..max_fd).filter(move |fd| !skip_fds.contains(fd))
}

/// Step 13: Close inherited file descriptors.
///
/// Iterates 3..rlim_cur, skipping fds in the skip list.
///
/// # Panics
///
/// Panics if getrlimit fails.
pub(crate) fn close_inherited_fds(skip_fds: &[i32]) {
    let max_fd = get_max_fd();
    for fd in fds_to_close(max_fd, skip_fds) {
        unsafe_ops::raw_close(fd);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use serial_test::serial;

    /// Guard that saves file descriptors on creation and restores them on drop.
    ///
    /// Tests that redirect stdout/stderr (fd 1/2) corrupt the test harness
    /// because the harness writes results to those fds. This guard `dup`s the
    /// originals before the test body runs, then `dup2`s them back when dropped.
    struct SavedFds {
        saved: Vec<(i32, i32)>, // (original_fd, saved_copy)
    }

    impl SavedFds {
        fn new(fds: &[i32]) -> Self {
            #[allow(unsafe_code)]
            let saved = fds
                .iter()
                .map(|&fd| {
                    let copy = unsafe { libc::dup(fd) };
                    assert!(copy >= 0, "dup({fd}) failed");
                    (fd, copy)
                })
                .collect();
            Self { saved }
        }

        /// Returns the raw fd numbers of the saved copies.
        ///
        /// Use this to build a skip list for `close_inherited_fds` so it
        /// doesn't close the backup copies we need for restoration.
        fn saved_fds(&self) -> Vec<i32> {
            self.saved.iter().map(|&(_, copy)| copy).collect()
        }
    }

    impl Drop for SavedFds {
        fn drop(&mut self) {
            #[allow(unsafe_code)]
            for &(orig, copy) in &self.saved {
                unsafe {
                    libc::dup2(copy, orig);
                    libc::close(copy);
                }
            }
        }
    }

    // --- Step 4: umask ---

    #[test]
    #[serial]
    fn set_umask_applies_and_can_be_read_back() {
        let old = nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
        set_umask(Mode::from_bits_truncate(0o022));
        let readback = nix::sys::stat::umask(old); // restore
        assert_eq!(readback, Mode::from_bits_truncate(0o022));
        nix::sys::stat::umask(old); // double-restore
    }

    // --- Step 5: chdir ---

    #[test]
    #[serial]
    fn change_dir_to_tempdir() {
        let original = std::env::current_dir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let result = change_dir(tmp.path());
        assert!(result.is_ok());
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(cwd, std::fs::canonicalize(tmp.path()).unwrap());
        std::env::set_current_dir(&original).unwrap();
    }

    #[test]
    #[serial]
    fn change_dir_nonexistent_fails() {
        let result = change_dir(Path::new("/nonexistent_daemonize_test_path"));
        assert!(matches!(result, Err(DaemonizeError::ChdirFailed(_))));
    }

    // --- Step 6: redirect to /dev/null ---

    #[test]
    #[serial]
    fn redirect_to_devnull_succeeds() {
        let _restore = SavedFds::new(&[0, 1, 2]);
        redirect_to_devnull();
    }

    // --- Step 7: open and lock ---

    #[test]
    fn open_and_lock_creates_and_locks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let flock = open_and_lock(&path).unwrap();
        assert!(path.exists());
        assert!(flock.as_raw_fd() >= 0);
    }

    #[test]
    fn open_and_lock_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let _first = open_and_lock(&path).unwrap();
        let second = open_and_lock(&path);
        assert!(matches!(second, Err(DaemonizeError::LockConflict(_))));
    }

    // --- Step 8: write pidfile ---

    #[test]
    fn write_pidfile_standalone() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        let result = write_pidfile(&pidfile, None, None);
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&pidfile).unwrap();
        let pid: u32 = contents.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn write_pidfile_shared_with_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.pid");
        let flock = open_and_lock(&path).unwrap();
        let result = write_pidfile(&path, Some(&path.clone()), Some(&flock));
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&path).unwrap();
        let pid: u32 = contents.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());
    }

    // --- Step 10: clear signal mask ---

    #[test]
    #[serial]
    fn clear_signal_mask_empties() {
        use nix::sys::signal::{SigSet, SigmaskHow, Signal};
        // Block SIGUSR1
        let mut set = SigSet::empty();
        set.add(Signal::SIGUSR1);
        nix::sys::signal::sigprocmask(SigmaskHow::SIG_BLOCK, Some(&set), None).unwrap();

        clear_signal_mask();

        let mut current = SigSet::empty();
        nix::sys::signal::sigprocmask(SigmaskHow::SIG_SETMASK, None, Some(&mut current)).unwrap();
        assert!(!current.contains(Signal::SIGUSR1));
    }

    // --- Step 11: set env vars ---

    #[test]
    #[serial]
    #[allow(unsafe_code)]
    fn set_env_vars_applies() {
        let vars = vec![
            ("DAEMONIZE_TEST_A".into(), "1".into()),
            ("DAEMONIZE_TEST_B".into(), "2".into()),
        ];
        set_env_vars(&vars);
        assert_eq!(std::env::var("DAEMONIZE_TEST_A").unwrap(), "1");
        assert_eq!(std::env::var("DAEMONIZE_TEST_B").unwrap(), "2");
        unsafe {
            std::env::remove_var("DAEMONIZE_TEST_A");
            std::env::remove_var("DAEMONIZE_TEST_B");
        }
    }

    #[test]
    #[serial]
    #[allow(unsafe_code)]
    fn set_env_vars_last_write_wins() {
        let vars = vec![
            ("DAEMONIZE_TEST_DUP".into(), "first".into()),
            ("DAEMONIZE_TEST_DUP".into(), "second".into()),
        ];
        set_env_vars(&vars);
        assert_eq!(std::env::var("DAEMONIZE_TEST_DUP").unwrap(), "second");
        unsafe { std::env::remove_var("DAEMONIZE_TEST_DUP") };
    }

    // --- Step 9: signal disposition reset ---

    #[test]
    #[serial]
    #[allow(unsafe_code)]
    fn reset_signal_dispositions_restores_default() {
        use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

        // Install SIG_IGN handler for SIGUSR1
        let handler = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        let old = unsafe { sigaction(Signal::SIGUSR1, &handler) }.unwrap();

        // Verify it's not SIG_DFL
        let current = unsafe { sigaction(Signal::SIGUSR1, &handler) }.unwrap();
        assert!(
            !matches!(current.handler(), SigHandler::SigDfl),
            "precondition: SIGUSR1 should not be SIG_DFL"
        );

        // Reset all dispositions
        crate::unsafe_ops::reset_signal_dispositions();

        // Read back SIGUSR1 disposition — should be SIG_DFL now
        let after_reset = unsafe { sigaction(Signal::SIGUSR1, &old) }.unwrap();
        assert!(
            matches!(after_reset.handler(), SigHandler::SigDfl),
            "SIGUSR1 should be SIG_DFL after reset"
        );

        // Restore original
        let _ = unsafe { sigaction(Signal::SIGUSR1, &old) };
    }

    // --- Step 13: redirect output (pure plan tests) ---

    #[test]
    fn plan_stdout_only_truncate() {
        let path = PathBuf::from("/tmp/out.log");
        let plan = plan_output_redirect(Some(&path), None, false);
        assert_eq!(
            plan.stdout,
            StreamAction::OpenAndRedirect {
                path: path.clone(),
                flags: OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
                target_fd: 1,
            }
        );
        assert_eq!(plan.stderr, StreamAction::None);
    }

    #[test]
    fn plan_stderr_only() {
        let path = PathBuf::from("/tmp/err.log");
        let plan = plan_output_redirect(None, Some(&path), false);
        assert_eq!(plan.stdout, StreamAction::None);
        assert_eq!(
            plan.stderr,
            StreamAction::OpenAndRedirect {
                path: path.clone(),
                flags: OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
                target_fd: 2,
            }
        );
    }

    #[test]
    fn plan_both_different_paths() {
        let out = PathBuf::from("/tmp/out.log");
        let err = PathBuf::from("/tmp/err.log");
        let plan = plan_output_redirect(Some(&out), Some(&err), false);
        assert!(matches!(
            plan.stdout,
            StreamAction::OpenAndRedirect { target_fd: 1, .. }
        ));
        assert!(matches!(
            plan.stderr,
            StreamAction::OpenAndRedirect { target_fd: 2, .. }
        ));
    }

    #[test]
    fn plan_both_same_path() {
        let path = PathBuf::from("/tmp/combined.log");
        let plan = plan_output_redirect(Some(&path), Some(&path), false);
        assert!(matches!(
            plan.stdout,
            StreamAction::OpenAndRedirect { target_fd: 1, .. }
        ));
        assert_eq!(
            plan.stderr,
            StreamAction::DupFrom {
                source_fd: 1,
                target_fd: 2,
            }
        );
    }

    #[test]
    fn plan_append_flag() {
        let path = PathBuf::from("/tmp/out.log");
        let plan = plan_output_redirect(Some(&path), None, true);
        if let StreamAction::OpenAndRedirect { flags, .. } = plan.stdout {
            assert!(flags.contains(OFlag::O_APPEND));
            assert!(!flags.contains(OFlag::O_TRUNC));
        } else {
            panic!("expected OpenAndRedirect");
        }
    }

    #[test]
    fn plan_truncate_flag() {
        let path = PathBuf::from("/tmp/out.log");
        let plan = plan_output_redirect(Some(&path), None, false);
        if let StreamAction::OpenAndRedirect { flags, .. } = plan.stdout {
            assert!(flags.contains(OFlag::O_TRUNC));
            assert!(!flags.contains(OFlag::O_APPEND));
        } else {
            panic!("expected OpenAndRedirect");
        }
    }

    #[test]
    fn plan_neither() {
        let plan = plan_output_redirect(None, None, false);
        assert_eq!(plan.stdout, StreamAction::None);
        assert_eq!(plan.stderr, StreamAction::None);
    }

    // --- Step 13: redirect output (executor smoke tests, serial) ---

    #[test]
    #[serial]
    fn execute_redirect_creates_files() {
        let _restore = SavedFds::new(&[1, 2]);
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        let stderr_path = dir.path().join("stderr.log");
        let plan = plan_output_redirect(Some(&stdout_path), Some(&stderr_path), false);
        let result = execute_output_redirect(&plan);
        assert!(result.is_ok());
        assert!(stdout_path.exists());
        assert!(stderr_path.exists());
    }

    #[test]
    #[serial]
    fn execute_redirect_truncate_vs_append() {
        let _restore = SavedFds::new(&[1]);
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");

        // Truncate mode
        std::fs::write(&stdout_path, "old content\n").unwrap();
        redirect_output(Some(&stdout_path), None, false).unwrap();
        std::io::stdout().write_all(b"new content\n").unwrap();
        std::io::stdout().flush().unwrap();
        let content = std::fs::read_to_string(&stdout_path).unwrap();
        assert!(!content.contains("old content"), "should have truncated");
        assert!(content.contains("new content"));

        // Append mode
        redirect_output(Some(&stdout_path), None, true).unwrap();
        std::io::stdout().write_all(b"appended\n").unwrap();
        std::io::stdout().flush().unwrap();
        let content = std::fs::read_to_string(&stdout_path).unwrap();
        assert!(content.contains("new content"), "should preserve existing");
        assert!(content.contains("appended"));
    }

    #[test]
    #[serial]
    fn execute_redirect_dup_from() {
        let _restore = SavedFds::new(&[1, 2]);
        let dir = tempfile::tempdir().unwrap();
        let combined = dir.path().join("combined.log");
        let plan = plan_output_redirect(Some(&combined), Some(&combined), false);
        execute_output_redirect(&plan).unwrap();

        std::io::stdout().write_all(b"stdout\n").unwrap();
        std::io::stdout().flush().unwrap();
        std::io::stderr().write_all(b"stderr\n").unwrap();
        std::io::stderr().flush().unwrap();

        let content = std::fs::read_to_string(&combined).unwrap();
        assert!(content.contains("stdout"));
        assert!(content.contains("stderr"));
    }

    #[test]
    #[serial]
    fn execute_redirect_stderr_only() {
        let _restore = SavedFds::new(&[2]);
        let dir = tempfile::tempdir().unwrap();
        let stderr_path = dir.path().join("stderr.log");
        let plan = plan_output_redirect(None, Some(&stderr_path), false);
        execute_output_redirect(&plan).unwrap();
        assert!(stderr_path.exists());

        std::io::stderr().write_all(b"stderr content\n").unwrap();
        std::io::stderr().flush().unwrap();
        let content = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(content.contains("stderr content"));
    }

    // --- Step 14: close inherited fds (pure plan tests) ---

    #[test]
    fn fds_to_close_skips_correctly() {
        let result: Vec<i32> = fds_to_close(10, &[4, 7]).collect();
        assert_eq!(result, vec![3, 5, 6, 8, 9]);
    }

    #[test]
    fn fds_to_close_empty_skip() {
        let result: Vec<i32> = fds_to_close(6, &[]).collect();
        assert_eq!(result, vec![3, 4, 5]);
    }

    #[test]
    fn fds_to_close_all_skipped() {
        let result: Vec<i32> = fds_to_close(6, &[3, 4, 5]).collect();
        assert!(result.is_empty());
    }

    #[test]
    fn fds_to_close_max_below_3() {
        let result: Vec<i32> = fds_to_close(2, &[]).collect();
        assert!(result.is_empty());
    }

    // --- Step 14: close inherited fds (executor smoke test, serial) ---

    #[test]
    #[serial]
    fn close_inherited_fds_preserves_skipped() {
        if std::env::var("CI").is_ok() {
            // Closing fds in-process triggers systemd's safe_close() EBADF
            // assertion on Ubuntu CI runners. Integration tests cover this path.
            return;
        }
        // Save stdout/stderr so the test harness can still report results
        // after we close all non-skipped fds (which includes harness-internal fds).
        let restore = SavedFds::new(&[1, 2]);
        let (rd, wr) = nix::unistd::pipe().unwrap();
        let mut skip = vec![rd.as_raw_fd(), wr.as_raw_fd()];
        // Also skip the SavedFds backup copies so they survive for restoration.
        skip.extend(restore.saved_fds());
        close_inherited_fds(&skip);
        // Our pipe fds should still be open
        assert!(nix::unistd::write(&wr, b"ok").is_ok());
    }

    #[test]
    fn write_pidfile_with_different_lockfile_path() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        let lockfile_path = dir.path().join("test.lock");
        let flock = open_and_lock(&lockfile_path).unwrap();
        // lockfile_path differs from pidfile — should use std::fs::write path
        let result = write_pidfile(&pidfile, Some(&lockfile_path), Some(&flock));
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&pidfile).unwrap();
        let pid: u32 = contents.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());
    }
}
