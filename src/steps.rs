//! Post-fork daemonization steps.
//!
//! Each function corresponds to one step in the 15-step daemonization sequence
//! orchestrated by [`daemonize_inner`](crate::daemonize_inner). They are
//! collected here rather than inlined because each is independently testable
//! and the grouping mirrors the spec's step numbering.

use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use nix::fcntl::{open, Flock, FlockArg, OFlag};
use nix::sys::stat::Mode;

use crate::error::DaemonizeError;
use crate::unsafe_ops;
use crate::util::paths_same;

/// Step 4: Set process umask.
pub(crate) fn set_umask(mode: Mode) {
    nix::sys::stat::umask(mode);
}

/// Step 5: Change working directory.
pub(crate) fn change_dir(path: &Path) -> Result<(), DaemonizeError> {
    nix::unistd::chdir(path)
        .map_err(|e| DaemonizeError::ChdirFailed(format!("chdir to {}: {e}", path.display())))
}

/// Step 6: Redirect stdin, stdout, stderr to /dev/null.
///
/// # Panics
///
/// Panics if /dev/null cannot be opened or dup2 fails.
pub(crate) fn redirect_to_devnull() {
    let devnull_path = c"/dev/null";
    let devnull_raw = unsafe_ops::raw_open(devnull_path, libc::O_RDWR, 0)
        .expect("failed to open /dev/null");

    for &target_fd in &[0, 1, 2] {
        if devnull_raw != target_fd {
            unsafe_ops::raw_dup2(devnull_raw, target_fd)
                .expect("failed to dup2 /dev/null");
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
    .map_err(|e| DaemonizeError::LockfileError(format!("cannot open lockfile {}: {e}", path.display())))?;

    Flock::lock(fd, FlockArg::LockExclusiveNonblock).map_err(|(_fd, e)| {
        if e == nix::errno::Errno::EWOULDBLOCK {
            DaemonizeError::LockConflict(format!(
                "lockfile {} is already locked by another process",
                path.display()
            ))
        } else {
            DaemonizeError::LockfileError(format!("cannot lock {}: {e}", path.display()))
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
        // Write to already-locked fd: seek, truncate, write via raw fd
        let raw_fd = flock.as_raw_fd();
        unsafe_ops::raw_lseek(raw_fd, 0)
            .map_err(|e| DaemonizeError::PidfileError(format!("seek pidfile: {e}")))?;
        unsafe_ops::raw_ftruncate(raw_fd, 0)
            .map_err(|e| DaemonizeError::PidfileError(format!("truncate pidfile: {e}")))?;
        unsafe_ops::raw_write(raw_fd, content.as_bytes())
            .map_err(|e| DaemonizeError::PidfileError(format!("write pidfile: {e}")))?;
    } else {
        // Open, write, close
        std::fs::write(pidfile_path, content.as_bytes())
            .map_err(|e| DaemonizeError::PidfileError(format!("write pidfile {}: {e}", pidfile_path.display())))?;
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

/// Step 12: Switch to the specified user.
///
/// Resolves via getpwnam, then initgroups, setgid, setuid.
/// Sets USER, HOME, LOGNAME environment variables.
#[allow(unsafe_code)]
pub(crate) fn switch_user(username: &str) -> Result<(), DaemonizeError> {
    use nix::unistd::User;
    use std::ffi::CString;

    let user = User::from_name(username)
        .map_err(|e| DaemonizeError::UserNotFound(format!("getpwnam({username}): {e}")))?
        .ok_or_else(|| DaemonizeError::UserNotFound(format!("user not found: {username}")))?;

    let cname = CString::new(username)
        .map_err(|e| DaemonizeError::UserNotFound(format!("invalid username: {e}")))?;

    // initgroups (use libc directly — nix doesn't provide this on all platforms)
    unsafe_ops::raw_initgroups(&cname, user.gid.as_raw())
        .map_err(|e| DaemonizeError::PermissionDenied(format!("initgroups: {e}")))?;

    // setgid
    nix::unistd::setgid(user.gid)
        .map_err(|e| DaemonizeError::PermissionDenied(format!("setgid: {e}")))?;

    // setuid
    nix::unistd::setuid(user.uid)
        .map_err(|e| DaemonizeError::PermissionDenied(format!("setuid: {e}")))?;

    // Set USER, HOME, LOGNAME — these overwrite any .env() values
    // SAFETY: post-fork, single-threaded — no concurrent readers.
    unsafe {
        std::env::set_var("USER", username);
        std::env::set_var("HOME", &user.dir);
        std::env::set_var("LOGNAME", username);
    }

    Ok(())
}

/// Step 13: Redirect stdout/stderr to configured files.
///
/// Opened after user switching so files have correct ownership.
/// Same-path optimization: if stdout and stderr resolve to the same path,
/// open once for stdout and dup2 to stderr.
pub(crate) fn redirect_output(
    stdout: Option<&PathBuf>,
    stderr: Option<&PathBuf>,
    append: bool,
) -> Result<(), DaemonizeError> {
    let mut flags = OFlag::O_WRONLY | OFlag::O_CREAT;
    if append {
        flags |= OFlag::O_APPEND;
    } else {
        flags |= OFlag::O_TRUNC;
    }
    let mode = Mode::from_bits_truncate(0o644);

    let same_path = match (stdout, stderr) {
        (Some(out), Some(err)) => paths_same(out, err),
        _ => false,
    };

    if let Some(out_path) = stdout {
        let fd = open(out_path, flags, mode)
            .map_err(|e| DaemonizeError::OutputFileError(format!(
                "cannot open stdout file {}: {e}", out_path.display()
            )))?;
        let raw = fd.as_raw_fd();
        if raw != 1 {
            unsafe_ops::raw_dup2(raw, 1).map_err(|e| DaemonizeError::OutputFileError(format!(
                "dup2 stdout: {e}"
            )))?;
            unsafe_ops::raw_close(raw);
        }
        // Prevent OwnedFd destructor from closing the fd — ownership is
        // transferred to the target fd slot (1) via dup2, or the fd already
        // was 1 and must stay open.
        std::mem::forget(fd);
    }

    if let Some(err_path) = stderr {
        if same_path {
            unsafe_ops::raw_dup2(1, 2).map_err(|e| DaemonizeError::OutputFileError(format!(
                "dup2 stderr (same path): {e}"
            )))?;
        } else {
            let fd = open(err_path, flags, mode)
                .map_err(|e| DaemonizeError::OutputFileError(format!(
                    "cannot open stderr file {}: {e}", err_path.display()
                )))?;
            let raw = fd.as_raw_fd();
            if raw != 2 {
                unsafe_ops::raw_dup2(raw, 2).map_err(|e| DaemonizeError::OutputFileError(format!(
                    "dup2 stderr: {e}"
                )))?;
                unsafe_ops::raw_close(raw);
            }
            std::mem::forget(fd);
        }
    }

    Ok(())
}

/// Step 14: Close inherited file descriptors.
///
/// Iterates 3..rlim_cur, skipping fds in the skip list.
///
/// # Panics
///
/// Panics if getrlimit fails.
pub(crate) fn close_inherited_fds(skip_fds: &[i32]) {
    let limit = nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE)
        .expect("getrlimit(RLIMIT_NOFILE) failed");
    let max_fd = limit.0 as i32; // rlim_cur

    for fd in 3..max_fd {
        if skip_fds.contains(&fd) {
            continue;
        }
        unsafe_ops::raw_close(fd);
    }
}

#[cfg(test)]
mod tests {
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

    // --- Step 13: redirect output ---

    #[test]
    #[serial]
    fn redirect_output_creates_files() {
        let _restore = SavedFds::new(&[1, 2]);
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        let stderr_path = dir.path().join("stderr.log");
        let result = redirect_output(Some(&stdout_path), Some(&stderr_path), false);
        assert!(result.is_ok());
        assert!(stdout_path.exists());
        assert!(stderr_path.exists());
    }

    #[test]
    #[serial]
    fn redirect_output_truncate_overwrites() {
        let _restore = SavedFds::new(&[1]);
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        std::fs::write(&stdout_path, "old content\n").unwrap();

        redirect_output(Some(&stdout_path), None, false).unwrap();

        unsafe_ops::raw_write(1, b"new content\n").unwrap();

        let content = std::fs::read_to_string(&stdout_path).unwrap();
        assert!(!content.contains("old content"), "should have truncated");
        assert!(content.contains("new content"), "should have new content");
    }

    #[test]
    #[serial]
    fn redirect_output_append_preserves() {
        let _restore = SavedFds::new(&[1]);
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        std::fs::write(&stdout_path, "old content\n").unwrap();

        redirect_output(Some(&stdout_path), None, true).unwrap();

        unsafe_ops::raw_write(1, b"new content\n").unwrap();

        let content = std::fs::read_to_string(&stdout_path).unwrap();
        assert!(content.contains("old content"), "should preserve old content");
        assert!(content.contains("new content"), "should have new content");
    }

    // --- Step 14: close inherited fds ---

    #[test]
    #[serial]
    fn close_inherited_fds_preserves_skipped() {
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

    #[test]
    #[serial]
    fn redirect_output_stderr_only() {
        let _restore = SavedFds::new(&[2]);
        let dir = tempfile::tempdir().unwrap();
        let stderr_path = dir.path().join("stderr.log");
        let result = redirect_output(None, Some(&stderr_path), false);
        assert!(result.is_ok());
        assert!(stderr_path.exists());

        unsafe_ops::raw_write(2, b"stderr content\n").unwrap();
        let content = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(content.contains("stderr content"));
    }

    #[test]
    #[serial]
    fn redirect_output_same_path_uses_dup2() {
        let _restore = SavedFds::new(&[1, 2]);
        let dir = tempfile::tempdir().unwrap();
        let combined = dir.path().join("combined.log");
        let result = redirect_output(Some(&combined), Some(&combined), false);
        assert!(result.is_ok());

        unsafe_ops::raw_write(1, b"stdout\n").unwrap();
        unsafe_ops::raw_write(2, b"stderr\n").unwrap();

        let content = std::fs::read_to_string(&combined).unwrap();
        assert!(content.contains("stdout"), "should have stdout");
        assert!(content.contains("stderr"), "should have stderr");
    }
}
