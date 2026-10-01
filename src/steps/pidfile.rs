//! The lockfile (step 7) and the pidfile (step 8): opening, locking, and
//! writing the daemon's pid.

use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;

use nix::fcntl::{Flock, FlockArg, OFlag};
use nix::sys::stat::Mode;
use nix::unistd::Whence;

use super::*;
use crate::error::DaemonizeError;
use crate::file_identity::{file_id, paths_same, FileId};

/// Step 7: Open and exclusively lock a lockfile.
pub(crate) fn open_and_lock(path: &Path) -> Result<Flock<OwnedFd>, DaemonizeError> {
    let fd = open_above_stdio(
        path,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o644),
    )
    .map_err(|e| DaemonizeError::LockfileError(format!("cannot open {}: {e}", path.display())))?;

    Flock::lock(fd, FlockArg::LockExclusiveNonblock).map_err(|(_fd, e)| {
        if e == nix::errno::Errno::EWOULDBLOCK {
            DaemonizeError::LockConflict {
                path: path.to_path_buf(),
            }
        } else {
            DaemonizeError::LockfileError(format!("flock {}: {e}", path.display()))
        }
    })
}

/// Step 8, first half: take hold of the pidfile, without writing it yet.
///
/// The split is where ownership starts. Once this returns, the file on disk is
/// this process's — it holds the lock on it, or has just created or truncated
/// it — and a sequence that aborts afterwards owes its removal. If this fails,
/// the file is untouched and not this process's to remove: a standalone `open`
/// refused with `EACCES` leaves an existing pidfile exactly as it was.
pub(crate) fn open_pidfile<'a>(
    pidfile_path: &'a Path,
    lockfile: Option<(&Path, &'a Flock<OwnedFd>)>,
) -> Result<Pidfile<'a>, DaemonizeError> {
    // The lockfile is the pidfile: step 7 already opened and locked it.
    if let Some((lp, flock)) = lockfile {
        if paths_same(pidfile_path, lp) {
            return Ok(Pidfile::Locked(flock));
        }
    }

    #[cfg(test)]
    if crate::steps::failpoints::PIDFILE_OPEN_FAILS.injected() {
        return Err(DaemonizeError::PidfileError(format!(
            "open {}: injected failure",
            pidfile_path.display()
        )));
    }
    // Open explicitly with mode 0644 (R98); std::fs::write would create the
    // file 0666 & ~umask, violating the mandated pidfile permissions.
    let fd = open_above_stdio(
        pidfile_path,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o644),
    )
    .map_err(|e| DaemonizeError::PidfileError(format!("open {}: {e}", pidfile_path.display())))?;
    Ok(Pidfile::Opened {
        fd,
        path: pidfile_path,
    })
}

/// A pidfile this process has taken hold of — see [`open_pidfile`].
pub(crate) enum Pidfile<'a> {
    /// The pidfile is the lockfile, already open and locked by step 7.
    Locked(&'a Flock<OwnedFd>),
    /// A standalone pidfile, created or truncated by [`open_pidfile`].
    Opened { fd: OwnedFd, path: &'a Path },
}

impl Pidfile<'_> {
    /// Step 8, second half: write this process's PID, replacing any content.
    ///
    /// Returns the identity of the file written, so step 12 can refuse a
    /// stream that is this file under another name (R146).
    pub(crate) fn write_pid(self) -> Result<FileId, DaemonizeError> {
        let content = format!("{}\n", std::process::id());
        match self {
            Pidfile::Locked(flock) => {
                nix::unistd::lseek(flock.as_fd(), 0, Whence::SeekSet)
                    .map_err(|e| DaemonizeError::PidfileError(format!("seek: {e}")))?;
                nix::unistd::ftruncate(flock.as_fd(), 0)
                    .map_err(|e| DaemonizeError::PidfileError(format!("truncate: {e}")))?;
                inject_pidfile_write_failure()?;
                write_all_fd(flock.as_fd(), content.as_bytes())
                    .map_err(|e| DaemonizeError::PidfileError(format!("write: {e}")))?;
                file_id(flock.as_fd())
                    .map_err(|e| DaemonizeError::PidfileError(format!("fstat: {e}")))
            }
            Pidfile::Opened { fd, path } => {
                inject_pidfile_write_failure()?;
                write_all_fd(&fd, content.as_bytes()).map_err(|e| {
                    DaemonizeError::PidfileError(format!("write {}: {e}", path.display()))
                })?;
                file_id(&fd).map_err(|e| {
                    DaemonizeError::PidfileError(format!("fstat {}: {e}", path.display()))
                })
            }
        }
    }
}

/// Step 8 in one call, for the tests that exercise the step rather than the
/// abort bookkeeping around it.
#[cfg(test)]
pub(crate) fn write_pidfile(
    pidfile_path: &Path,
    lockfile: Option<(&Path, &Flock<OwnedFd>)>,
) -> Result<(), DaemonizeError> {
    open_pidfile(pidfile_path, lockfile)?.write_pid().map(drop)
}

/// Stands in for a write that fails once the pidfile already exists and is
/// empty. Compiled away outside tests.
#[inline]
fn inject_pidfile_write_failure() -> Result<(), DaemonizeError> {
    #[cfg(test)]
    if crate::steps::failpoints::PIDFILE_WRITE_FAILS.injected() {
        return Err(DaemonizeError::PidfileError(
            "write: injected failure".to_string(),
        ));
    }
    Ok(())
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

/// Removes the pidfile this process wrote if the sequence never reaches the end.
///
/// Everything from the second half of step 8 through step 17 runs after this
/// process has taken hold of the pidfile, and any of it can fail. In daemon mode the abort path signals the parent and `_exit`s, and in
/// foreground mode the error returns to the caller; neither builds a
/// [`DaemonContext`](crate::DaemonContext), so no drop-time cleanup ever runs. Without this the file
/// is left naming a process that never started — what `kill $(cat …)`, a status
/// script, or systemd's `PIDFile=` reads next, and after PID reuse some other
/// process entirely.
///
/// Armed once [`open_pidfile`] returns, which is when the file becomes
/// this process's — not before, so a lock conflict or a refused `open` cannot
/// remove a file that belongs to someone else, and not after the write, so a
/// write that fails half way does not leave an empty pidfile behind. Gated on
/// [`cleanup_on_drop`](crate::DaemonConfig::cleanup_on_drop), matching the drop-time
/// cleanup that [`DaemonContext::report_error`](crate::DaemonContext::report_error) replicates for the same reason.
pub(crate) struct PidfileOnAbort<'a> {
    path: Option<&'a std::path::Path>,
}

impl<'a> PidfileOnAbort<'a> {
    /// A guard that removes nothing until [`arm`](Self::arm)ed.
    pub(crate) fn new() -> Self {
        Self { path: None }
    }

    /// From now on, an abort removes the pidfile at `path`.
    pub(crate) fn arm(&mut self, path: &'a std::path::Path) {
        self.path = Some(path);
    }

    /// The sequence reached the end; the [`DaemonContext`](crate::DaemonContext) owns the pidfile now.
    pub(crate) fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for PidfileOnAbort<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {

    use std::os::fd::AsRawFd;
    use std::path::PathBuf;

    use super::*;

    use crate::test_support::{is_subprocess, run_in_subprocess};

    // Covers: R96
    #[test]
    fn open_and_lock_creates_and_locks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let flock = open_and_lock(&path).unwrap();
        assert!(path.exists());
        assert!(flock.as_raw_fd() >= 0);
    }

    // Covers: R152
    #[test]
    fn lockfile_and_pidfile_stay_clear_of_stdio() {
        crate::test_support::run_in_subprocess(
            "steps::pidfile::tests::lockfile_and_pidfile_stay_clear_of_stdio_subprocess",
        );
    }

    /// In foreground mode step 6 leaves a closed stdout closed, so `open`
    /// would hand the lockfile fd 1, and step 12's redirect of stdout would
    /// close it and drop the lock.
    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn lockfile_and_pidfile_stay_clear_of_stdio_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // Stdout and stderr closed gives the first open fd 1; only stderr
        // closed gives it fd 2 itself, the boundary. Both files stay open
        // while the other is opened, so neither reuses the other's slot.
        for (round, closed) in [&[1, 2][..], &[2][..]].into_iter().enumerate() {
            let lock_path = dir.path().join(format!("{round}.lock"));
            let pid_path = dir.path().join(format!("{round}.pid"));
            let saved: Vec<(i32, i32)> = closed
                .iter()
                .map(|&fd| (fd, unsafe { libc::dup(fd) }))
                .collect();
            for &fd in closed {
                unsafe { libc::close(fd) };
            }

            let lock = open_and_lock(&lock_path);
            let pid = open_pidfile(&pid_path, None);

            for (fd, copy) in saved {
                unsafe { libc::dup2(copy, fd) };
                unsafe { libc::close(copy) };
            }
            let lock = lock.unwrap();
            let pid = match pid.unwrap() {
                Pidfile::Opened { fd, .. } => fd.as_raw_fd(),
                Pidfile::Locked(_) => unreachable!("no lockfile given"),
            };
            assert!(
                lock.as_raw_fd() > 2 && pid > 2,
                "with {closed:?} closed: lockfile fd {}, pidfile fd {pid}",
                lock.as_raw_fd()
            );
        }
    }

    #[test]
    fn open_and_lock_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let _first = open_and_lock(&path).unwrap();
        let second = open_and_lock(&path);
        match second {
            Err(DaemonizeError::LockConflict { path: conflicting }) => {
                assert_eq!(conflicting, path);
            }
            other => panic!("expected LockConflict, got {other:?}"),
        }
    }

    #[test]
    fn lock_conflict_display_names_the_path() {
        let err = DaemonizeError::LockConflict {
            path: PathBuf::from("/run/app.pid"),
        };
        assert_eq!(
            err.to_string(),
            "lock conflict: /run/app.pid is already locked by another process"
        );
    }

    #[test]
    fn write_pidfile_standalone() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        let result = write_pidfile(&pidfile, None);
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&pidfile).unwrap();
        let pid: u32 = contents.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());
    }

    // Covers: R98
    #[test]
    fn write_pidfile_standalone_mode_is_0644() {
        run_in_subprocess(
            "steps::pidfile::tests::write_pidfile_standalone_mode_is_0644_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn write_pidfile_standalone_mode_is_0644_subprocess() {
        use std::os::unix::fs::PermissionsExt;

        if !is_subprocess() {
            return;
        }
        // umask 0 so the on-disk mode reflects the open()/create mode exactly,
        // not umask masking. std::fs::write creates 0666; R98 mandates 0644.
        // Set directly rather than through set_umask: with a set_umask that
        // did nothing, the inherited 022 would turn 0666 into 0644 too.
        nix::sys::stat::umask(Mode::empty());
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("mode.pid");
        write_pidfile(&pidfile, None).unwrap();
        let mode = std::fs::metadata(&pidfile).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o644,
            "standalone pidfile must be created 0644, got {mode:o}"
        );
    }

    #[test]
    fn write_pidfile_standalone_truncates_stale_content() {
        // A stale pidfile longer than the new PID must not keep a garbage
        // tail (O_TRUNC): "999999999999\n" overwritten by pid 42 must read
        // "42\n", not "42\n9999999999\n".
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("stale.pid");
        std::fs::write(&pidfile, "999999999999999999\n").unwrap();
        write_pidfile(&pidfile, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&pidfile).unwrap(),
            format!("{}\n", std::process::id())
        );
    }

    #[test]
    fn write_pidfile_standalone_open_error() {
        let result = write_pidfile(Path::new("/nonexistent_blivet_dir/x.pid"), None);
        assert!(matches!(result, Err(DaemonizeError::PidfileError(msg)) if msg.contains("open")));
    }

    #[test]
    fn write_pidfile_shared_with_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.pid");
        let flock = open_and_lock(&path).unwrap();
        let result = write_pidfile(&path, Some((path.as_path(), &flock)));
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&path).unwrap();
        let pid: u32 = contents.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn write_pidfile_shared_writes_through_locked_fd_not_path() {
        // When the pidfile shares the lockfile path, write_pidfile must reuse the
        // already-locked fd (seek + truncate + write), *not* re-open the path.
        // Both routes leave identical file *content*, so a content check alone
        // cannot tell them apart — a mutant that forces the standalone
        // `fs::write` branch survives it. Pin the distinguishing behavior:
        // unlink the path after locking, leaving the held fd pointing at the
        // now-orphaned inode. Writing through that fd leaves the directory entry
        // gone; the standalone branch would `O_CREAT` the path back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.pid");
        let flock = open_and_lock(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        write_pidfile(&path, Some((path.as_path(), &flock))).unwrap();

        assert!(
            !path.exists(),
            "shared path must write through the locked fd, not re-create the pidfile at its path"
        );
    }

    #[test]
    fn write_pidfile_with_different_lockfile_path() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        let lockfile_path = dir.path().join("test.lock");
        let flock = open_and_lock(&lockfile_path).unwrap();
        // lockfile_path differs from pidfile — should use std::fs::write path
        let result = write_pidfile(&pidfile, Some((lockfile_path.as_path(), &flock)));
        assert!(result.is_ok());
        let contents = std::fs::read_to_string(&pidfile).unwrap();
        let pid: u32 = contents.trim().parse().unwrap();
        assert_eq!(pid, std::process::id());
    }
}
