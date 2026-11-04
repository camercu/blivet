//! The lockfile (step 7) and the pidfile (step 8): opening, locking, and
//! writing the daemon's pid.

use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;

use nix::fcntl::{Flock, FlockArg, OFlag};
use nix::sys::stat::Mode;
use nix::unistd::Whence;

use super::*;
use crate::error::ErrorKind;
use crate::file_identity::{
    FileId, file_id, is_symlink, lstat_if_named, paths_same, stat_if_named,
};

/// Step 7: Open and exclusively lock a lockfile, saying whether this start
/// created it.
///
/// A rollback, or a daemon's cleanup, unlinks the file it locked, so the file
/// this opened may be gone from the path by the time the lock is taken. That
/// lock would guard nothing: the next instance creates the path anew and locks
/// that. So the lock counts only once the path still names the locked file;
/// else the step opens the path again, up to [`LOCK_ATTEMPTS`] times.
///
/// A lockfile derived from the pidfile is opened as the pidfile is, refusing
/// a symlink (see [`open_pidfile`]). An explicit one may be a symlink.
///
/// Returns the lock, whether this start created the file, and its identity.
pub(crate) fn lock_path(
    path: &Path,
    derived: bool,
) -> Result<(Flock<OwnedFd>, bool, FileId), ErrorKind> {
    for _ in 0..LOCK_ATTEMPTS {
        let (flock, created) = lock_once(path, derived)?;
        let locked = file_id(&*flock)
            .map_err(|e| ErrorKind::LockfileError(format!("fstat {}: {e}", path.display())))?;
        let named = stat_if_named(path, locked)
            .map_err(|e| ErrorKind::LockfileError(format!("stat {}: {e}", path.display())))?;
        if named.is_some() {
            return Ok((flock, created, locked));
        }
    }
    Err(ErrorKind::LockfileError(format!(
        "{} kept changing while it was locked",
        path.display()
    )))
}

/// [`lock_path`], for the tests that want only the lock.
#[cfg(test)]
pub(crate) fn open_and_lock(path: &Path) -> Result<Flock<OwnedFd>, ErrorKind> {
    lock_path(path, false).map(|(flock, ..)| flock)
}

/// How often step 7 opens the lockfile again when the path stopped naming the
/// file it locked. Each retry means another instance removed the file in the
/// moment between this one's `open` and `flock`, so a few suffice.
const LOCK_ATTEMPTS: usize = 8;

/// Opens the lockfile and takes its lock, without checking the path.
fn lock_once(path: &Path, derived: bool) -> Result<(Flock<OwnedFd>, bool), ErrorKind> {
    let flags = if derived {
        OFlag::O_WRONLY | OFlag::O_NOFOLLOW
    } else {
        OFlag::O_WRONLY
    };
    let (fd, created) = open_creating(path, flags, Mode::from_bits_truncate(0o644))
        .map_err(|e| ErrorKind::LockfileError(open_failure(path, &e)))?;

    #[cfg(test)]
    crate::steps::failpoints::unlink_if_injected(path);

    let flock = Flock::lock(fd, FlockArg::LockExclusiveNonblock).map_err(|(_fd, e)| {
        if e == nix::errno::Errno::EWOULDBLOCK {
            ErrorKind::LockConflict {
                path: path.to_path_buf(),
            }
        } else {
            ErrorKind::LockfileError(format!("flock {}: {e}", path.display()))
        }
    })?;
    Ok((flock, created))
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
) -> Result<Pidfile<'a>, ErrorKind> {
    // The lockfile is the pidfile: step 7 already opened and locked it. The
    // pidfile path must still be that file itself, not a link to it, for the
    // rollback to remove it.
    if let Some((lp, flock)) = lockfile {
        if paths_same(pidfile_path, lp) {
            let locked = file_id(flock.as_fd())
                .map_err(|e| ErrorKind::PidfileError(format!("fstat: {e}")))?;
            return match lstat_if_named(pidfile_path, locked) {
                Ok(Some(_)) => Ok(Pidfile::Locked(flock)),
                Ok(None) => Err(ErrorKind::PidfileError(not_the_locked_file(pidfile_path))),
                Err(e) => Err(ErrorKind::PidfileError(format!(
                    "lstat {}: {e}",
                    pidfile_path.display()
                ))),
            };
        }
    }

    #[cfg(test)]
    if crate::steps::failpoints::PIDFILE_OPEN_FAILS.injected() {
        return Err(ErrorKind::PidfileError(format!(
            "open {}: injected failure",
            pidfile_path.display()
        )));
    }
    // Open explicitly with mode 0644 (R98); std::fs::write would create the
    // file 0666 & ~umask, violating the mandated pidfile permissions. A
    // symlink is refused: a failed start removes the pidfile only while the
    // path itself is the file it wrote (R164), and could not remove one
    // written through a link.
    let fd = open_above_stdio(
        pidfile_path,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC | OFlag::O_NOFOLLOW,
        Mode::from_bits_truncate(0o644),
    )
    .map_err(|e| ErrorKind::PidfileError(open_failure(pidfile_path, &e)))?;
    Ok(Pidfile::Opened {
        fd,
        path: pidfile_path,
    })
}

/// Why opening `path` failed. A refusal under `O_NOFOLLOW` is `ELOOP` on
/// Linux and macOS but `EMLINK` on FreeBSD, so the path says what it is.
fn open_failure(path: &Path, e: &std::io::Error) -> String {
    if is_symlink(path) {
        not_the_file(path)
    } else {
        format!("cannot open {}: {e}", path.display())
    }
}

/// The refusal of a pidfile path that is a symlink.
fn not_the_file(path: &Path) -> String {
    format!(
        "{} is a symlink: name the file it points to, so a failed start can remove it",
        path.display()
    )
}

/// The refusal of a pidfile path that no longer names the file step 7 locked:
/// a symlink to it, or a path another process removed or replaced since.
fn not_the_locked_file(path: &Path) -> String {
    if is_symlink(path) {
        not_the_file(path)
    } else {
        format!(
            "{} no longer names the file locked at step 7",
            path.display()
        )
    }
}

/// A pidfile this process has taken hold of — see [`open_pidfile`].
pub(crate) enum Pidfile<'a> {
    /// The pidfile is the lockfile, already open and locked by step 7.
    Locked(&'a Flock<OwnedFd>),
    /// A standalone pidfile, created or truncated by [`open_pidfile`].
    Opened { fd: OwnedFd, path: &'a Path },
}

impl Pidfile<'_> {
    /// The identity of the file taken hold of, for its removal on failure.
    pub(crate) fn id(&self) -> Result<FileId, ErrorKind> {
        let fd = match self {
            Pidfile::Locked(flock) => flock.as_fd(),
            Pidfile::Opened { fd, .. } => fd.as_fd(),
        };
        file_id(fd).map_err(|e| ErrorKind::PidfileError(format!("fstat: {e}")))
    }

    /// Step 8, second half: write this process's PID, replacing any content.
    ///
    /// Returns a standalone pidfile's descriptor, which step 16 chowns
    /// (R163). A pidfile that is the lockfile has none: the lockfile's
    /// descriptor serves. The write keeps the file's [`id`](Self::id).
    pub(crate) fn write_pid(self) -> Result<Option<OwnedFd>, ErrorKind> {
        let content = format!("{}\n", std::process::id());
        match self {
            Pidfile::Locked(flock) => {
                nix::unistd::lseek(flock.as_fd(), 0, Whence::SeekSet)
                    .map_err(|e| ErrorKind::PidfileError(format!("seek: {e}")))?;
                nix::unistd::ftruncate(flock.as_fd(), 0)
                    .map_err(|e| ErrorKind::PidfileError(format!("truncate: {e}")))?;
                inject_pidfile_write_failure()?;
                write_all_fd(flock.as_fd(), content.as_bytes())
                    .map_err(|e| ErrorKind::PidfileError(format!("write: {e}")))?;
                Ok(None)
            }
            Pidfile::Opened { fd, path } => {
                inject_pidfile_write_failure()?;
                write_all_fd(&fd, content.as_bytes()).map_err(|e| {
                    ErrorKind::PidfileError(format!("write {}: {e}", path.display()))
                })?;
                Ok(Some(fd))
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
) -> Result<(), ErrorKind> {
    open_pidfile(pidfile_path, lockfile)?.write_pid().map(drop)
}

/// Stands in for a write that fails once the pidfile already exists and is
/// empty. Compiled away outside tests.
#[inline]
fn inject_pidfile_write_failure() -> Result<(), ErrorKind> {
    #[cfg(test)]
    if crate::steps::failpoints::PIDFILE_WRITE_FAILS.injected() {
        return Err(ErrorKind::PidfileError(
            "write: injected failure".to_string(),
        ));
    }
    Ok(())
}

/// Write all bytes to a file descriptor, looping on partial writes.
#[allow(clippy::disallowed_methods)] // the pidfile, a regular file: SIGPIPE cannot hit it
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

#[cfg(test)]
mod tests {

    use std::os::fd::AsRawFd;
    use std::path::PathBuf;

    use super::*;

    use crate::test_support::isolated;

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
    /// In foreground mode step 6 leaves a closed stdout closed, so `open`
    /// would hand the lockfile fd 1, and step 12's redirect of stdout would
    /// close it and drop the lock.
    #[test]
    fn lockfile_and_pidfile_stay_clear_of_stdio() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // Stdout and stderr closed gives the first open fd 1; only stderr
        // closed gives it fd 2 itself, the boundary. Both files stay open
        // while the other is opened, so neither reuses the other's slot.
        for (round, closed) in [&[1, 2][..], &[2][..]].into_iter().enumerate() {
            let lock_path = dir.path().join(format!("{round}.lock"));
            let pid_path = dir.path().join(format!("{round}.pid"));
            let (lock, pid) = crate::test_support::with_fds_closed(closed, || {
                (open_and_lock(&lock_path), open_pidfile(&pid_path, None))
            });
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

    // Covers: R169
    /// Another instance's rollback can unlink the lockfile after this one
    /// opened it and before it locked it, and a third instance create the
    /// path anew. A lock on the unlinked file guards nothing.
    #[test]
    fn the_lock_held_is_on_the_file_the_path_names() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        crate::steps::failpoints::unlink_before_lock(1, true);
        let flock = open_and_lock(&path).unwrap();
        assert_eq!(
            file_id(&*flock).ok(),
            nix::sys::stat::stat(&path).ok().map(|st| FileId::of(&st)),
        );
    }

    // Covers: R169
    /// A lockfile unlinked under every attempt fails the step instead of
    /// spinning.
    #[test]
    fn a_lockfile_that_keeps_vanishing_fails_the_lock() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        crate::steps::failpoints::unlink_before_lock(usize::MAX, false);
        let result = open_and_lock(&path);
        assert!(
            matches!(&result, Err(ErrorKind::LockfileError(m)) if m.contains("kept changing")),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn open_and_lock_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let _first = open_and_lock(&path).unwrap();
        let second = open_and_lock(&path);
        match second {
            Err(ErrorKind::LockConflict { path: conflicting }) => {
                assert_eq!(conflicting, path);
            }
            other => panic!("expected LockConflict, got {other:?}"),
        }
    }

    #[test]
    fn lock_conflict_display_names_the_path() {
        let err = ErrorKind::LockConflict {
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
        use std::os::unix::fs::PermissionsExt;

        if !isolated() {
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
        assert!(matches!(result, Err(ErrorKind::PidfileError(msg)) if msg.contains("open")));
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
    fn write_pidfile_shared_never_reopens_the_path() {
        // When the pidfile shares the lockfile path, write_pidfile must reuse the
        // already-locked fd, *not* re-open the path. Both routes leave identical
        // file *content*, so a content check alone cannot tell them apart — a
        // mutant that forces the standalone branch survives it. Pin the
        // distinguishing behavior: unlink the path after locking. The shared
        // route refuses the path that no longer names the locked file; the
        // standalone branch would `O_CREAT` the path back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.pid");
        let flock = open_and_lock(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        let _ = write_pidfile(&path, Some((path.as_path(), &flock)));

        assert!(
            !path.exists(),
            "shared path must not re-create the pidfile at its path"
        );
    }

    #[test]
    fn a_shared_pidfile_unlinked_after_locking_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.pid");
        let flock = open_and_lock(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        let result = write_pidfile(&path, Some((path.as_path(), &flock)));

        assert!(
            matches!(&result, Err(ErrorKind::PidfileError(m)) if m.contains("no longer names")),
            "{result:?}"
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
