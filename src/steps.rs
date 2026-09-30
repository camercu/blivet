//! Post-fork daemonization steps.
//!
//! Each function corresponds to one step in the daemonization sequence
//! orchestrated by [`daemonize_inner`](crate::daemonize_inner). They are
//! collected here rather than inlined because each is independently testable.

use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::Path;

use nix::fcntl::{open, Flock, FlockArg, OFlag};
use nix::sys::stat::Mode;
use nix::unistd::{self, Whence};

use crate::error::DaemonizeError;
use crate::unsafe_ops;
use crate::util::paths_same;

/// Test-only failure injection for post-fork syscalls that cannot be made to
/// fail from inside a test process (that would need a missing `/dev/null` or a
/// seccomp filter). A set flag makes its step take the error return so tests
/// can pin that the failure *propagates* out of the sequence rather than being
/// swallowed. Flags are process-global: a test that sets one must run in an
/// isolated subprocess (`test_support::run_in_subprocess`).
#[cfg(test)]
pub(crate) mod failpoints {
    use std::sync::atomic::AtomicBool;

    pub(crate) static DEVNULL_OPEN_FAILS: AtomicBool = AtomicBool::new(false);
    pub(crate) static SIGACTION_FAILS: AtomicBool = AtomicBool::new(false);
    pub(crate) static SIGPROCMASK_FAILS: AtomicBool = AtomicBool::new(false);
    pub(crate) static GETRLIMIT_FAILS: AtomicBool = AtomicBool::new(false);
    pub(crate) static FD_LISTING_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

    /// Fails the pidfile write *after* the file has been created and
    /// truncated, which is the shape of a real `ENOSPC`/`EIO`. The step-9-13
    /// failpoints cannot reach this: by the time they fire, step 8 has already
    /// returned `Ok`.
    pub(crate) static PIDFILE_WRITE_FAILS: AtomicBool = AtomicBool::new(false);

    /// Fails the standalone pidfile `open` itself, before this process has
    /// touched the file — an `EACCES`, say, that root would not hit. What is on
    /// disk then is not this process's to remove.
    pub(crate) static PIDFILE_OPEN_FAILS: AtomicBool = AtomicBool::new(false);

    /// Fails step 12's truncation of a stream file, the last thing it does:
    /// every stdio slot has moved by then.
    pub(crate) static STREAM_TRUNCATE_FAILS: AtomicBool = AtomicBool::new(false);

    /// Stands in for the fd limit `getrlimit` would report, or 0 to use the
    /// real one.
    ///
    /// A test that wants the brute-force branch wants the branch, not the
    /// machine's `RLIMIT_NOFILE`: closing `3..rlim_cur` is a real loop, and a
    /// host with a large limit makes an otherwise instant test take minutes
    /// (measured: ~0.2s at 1M, and a systemd `LimitNOFILE=infinity` clamps to
    /// `i32::MAX`). Bounding it here keeps the test constant-time on any host.
    pub(crate) static MAX_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

    /// True when `flag` is set — reads with `Relaxed`: flags are set before the
    /// sequence runs and never concurrently.
    pub(crate) fn injected(flag: &AtomicBool) -> bool {
        flag.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The fd limit [`MAX_FD`] stands in with, if a test set one.
    pub(crate) fn injected_max_fd() -> Option<i32> {
        let max_fd = MAX_FD.load(std::sync::atomic::Ordering::Relaxed);
        (max_fd > 0).then_some(max_fd)
    }
}

// ---- File identity, for step 12's same-file checks ----

/// A file's identity: the device and inode `fstat` reports. Two paths, or two
/// descriptors, name the same file exactly when these match — however they
/// were spelled, through whatever symlinks, on whatever filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileId {
    dev: libc::dev_t,
    ino: libc::ino_t,
}

/// The identity of the open file `fd`.
pub(crate) fn file_id(fd: impl AsFd) -> Result<FileId, nix::errno::Errno> {
    nix::sys::stat::fstat(fd).map(|st| FileId::of(&st))
}

impl FileId {
    /// The identity `fstat` reported.
    fn of(st: &nix::sys::stat::FileStat) -> Self {
        FileId {
            dev: st.st_dev,
            ino: st.st_ino,
        }
    }
}

/// What step 12 does with the streams it has opened — see
/// [`plan_output_redirect`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RedirectPlan {
    /// stderr is the same file as stdout, so it takes stdout's descriptor
    /// rather than a second one with an offset of its own.
    pub(crate) stderr_shares_stdout: bool,
}

/// A stream that turned out to be a file the sequence owns.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Collision {
    pub(crate) stream: &'static str,
    pub(crate) owned: &'static str,
}

/// Step 4: Set process umask.
///
/// `mode` is an octal permission value (validated `<= 0o7777` by
/// [`DaemonConfig::validate`](crate::DaemonConfig::validate)); the cast to
/// `mode_t` is therefore lossless.
pub(crate) fn set_umask(mode: u32) {
    #[cfg(test)]
    crate::test_support::assert_isolated("set_umask");
    nix::sys::stat::umask(Mode::from_bits_truncate(mode as libc::mode_t));
}

/// Step 5: Change working directory.
pub(crate) fn change_dir(path: &Path) -> Result<(), DaemonizeError> {
    #[cfg(test)]
    crate::test_support::assert_isolated("change_dir");
    nix::unistd::chdir(path)
        .map_err(|e| DaemonizeError::ChdirFailed(format!("{}: {e}", path.display())))
}

/// Opens `path`, never on fd 0, 1 or 2.
///
/// With a stdio slot closed, `open` returns that slot itself. The `dup2` onto
/// it that follows would then be a no-op, and dropping the descriptor would
/// close the redirect it had just made. `try_clone` duplicates with a floor of
/// 3, so the source of a redirect is never one of its targets and dropping it
/// is always safe.
fn open_above_stdio<P: ?Sized + nix::NixPath>(
    path: &P,
    flags: OFlag,
    mode: Mode,
) -> std::io::Result<OwnedFd> {
    let fd = open(path, flags, mode)?;
    if fd.as_raw_fd() <= 2 {
        fd.try_clone()
    } else {
        Ok(fd)
    }
}

/// Step 6: Redirect standard streams to /dev/null.
///
/// Always redirects stdin. When `stdout_stderr` is true, also redirects
/// stdout and stderr. In foreground mode, stdout/stderr are left
/// inherited so output reaches the parent terminal or supervisor.
///
/// Returns [`SystemError`](DaemonizeError::SystemError) if `/dev/null` cannot
/// be opened or a `dup2` fails (e.g. a minimal container with no `/dev/null`),
/// so the caller can report the failure to the parent rather than crashing.
#[allow(clippy::disallowed_methods)] // a stdio mover; asserts isolation under test
pub(crate) fn redirect_to_devnull(stdout_stderr: bool) -> Result<(), DaemonizeError> {
    #[cfg(test)]
    crate::test_support::assert_isolated("redirect_to_devnull");
    #[cfg(test)]
    if failpoints::injected(&failpoints::DEVNULL_OPEN_FAILS) {
        return Err(DaemonizeError::SystemError(
            "open /dev/null: injected failure".into(),
        ));
    }
    let devnull = open_above_stdio(c"/dev/null", OFlag::O_RDWR, Mode::empty())
        .map_err(|e| DaemonizeError::SystemError(format!("open /dev/null: {e}")))?;
    unistd::dup2_stdin(&devnull)
        .map_err(|e| DaemonizeError::SystemError(format!("dup2 /dev/null -> stdin: {e}")))?;
    if stdout_stderr {
        unistd::dup2_stdout(&devnull)
            .map_err(|e| DaemonizeError::SystemError(format!("dup2 /dev/null -> stdout: {e}")))?;
        unistd::dup2_stderr(&devnull)
            .map_err(|e| DaemonizeError::SystemError(format!("dup2 /dev/null -> stderr: {e}")))?;
    }
    Ok(())
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
    if failpoints::injected(&failpoints::PIDFILE_OPEN_FAILS) {
        return Err(DaemonizeError::PidfileError(format!(
            "open {}: injected failure",
            pidfile_path.display()
        )));
    }
    // Open explicitly with mode 0644 (R98); std::fs::write would create the
    // file 0666 & ~umask, violating the mandated pidfile permissions.
    let fd = open(
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
    if failpoints::injected(&failpoints::PIDFILE_WRITE_FAILS) {
        return Err(DaemonizeError::PidfileError(
            "write: injected failure".to_string(),
        ));
    }
    Ok(())
}

/// Step 10: Clear the signal mask.
///
/// Returns [`SystemError`](DaemonizeError::SystemError) if `sigprocmask` fails
/// (e.g. blocked by a seccomp filter) so the caller can report it.
pub(crate) fn clear_signal_mask() -> Result<(), DaemonizeError> {
    use nix::sys::signal::{SigSet, SigmaskHow};
    #[cfg(test)]
    if failpoints::injected(&failpoints::SIGPROCMASK_FAILS) {
        return Err(DaemonizeError::SystemError(
            "sigprocmask: injected failure".into(),
        ));
    }
    nix::sys::signal::sigprocmask(SigmaskHow::SIG_SETMASK, Some(&SigSet::empty()), None)
        .map_err(|e| DaemonizeError::SystemError(format!("sigprocmask: {e}")))
}

/// Step 11: Set environment variables in insertion order.
///
/// Uses `std::env::set_var`, which is not thread-safe. Sound here because this
/// runs only inside the daemonization sequence: post-fork the child is
/// single-threaded by `fork` semantics, and in foreground mode the entry point
/// ([`daemonize`](crate::daemonize) /
/// [`daemonize_unchecked`](crate::daemonize_unchecked)) requires
/// single-threadedness. No other thread can touch `environ`.
///
/// Infallible for a validated config: `set_var` panics only on an empty key,
/// `=` or NUL in the key, or NUL in the value — all rejected by
/// [`DaemonConfig::validate`](crate::DaemonConfig::validate) (R36, R138).
#[allow(unsafe_code)]
pub(crate) fn set_env_vars(env: &[(String, String)]) {
    #[cfg(test)]
    crate::test_support::assert_isolated("set_env_vars");
    for (key, value) in env {
        // SAFETY: single-threaded per this fn's contract (post-fork child or
        // foreground entry gate), so the `setenv` cannot race.
        unsafe { std::env::set_var(key, value) };
    }
}

/// Decides step 12 from the identities of the files it opened.
///
/// Pure: no descriptor is touched here. A stream that is a file the sequence
/// owns — the pidfile, the lockfile — is refused, because redirecting into it
/// would overwrite what the owner wrote. stdout and stderr may share a file,
/// in which case they share a descriptor.
pub(crate) fn plan_output_redirect(
    stdout: Option<FileId>,
    stderr: Option<FileId>,
    owned: &[(&'static str, FileId)],
) -> Result<RedirectPlan, Collision> {
    for (stream, id) in [("stdout", stdout), ("stderr", stderr)] {
        let Some(id) = id else { continue };
        if let Some((owned, _)) = owned.iter().find(|(_, o)| *o == id) {
            return Err(Collision { stream, owned });
        }
    }
    Ok(RedirectPlan {
        stderr_shares_stdout: stdout.is_some() && stdout == stderr,
    })
}

/// A stream's file, opened but not yet truncated or redirected.
struct OpenStream<'a> {
    path: &'a Path,
    fd: OwnedFd,
    id: FileId,
    regular: bool,
}

/// Opens `path` for a stream without truncating it: nothing on disk may
/// change until [`plan_output_redirect`] has seen what the file is.
fn open_stream(path: &Path, append: bool) -> Result<OpenStream<'_>, DaemonizeError> {
    let mut flags = OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_CLOEXEC;
    if append {
        flags |= OFlag::O_APPEND;
    }
    let fd = open_above_stdio(path, flags, Mode::from_bits_truncate(0o644)).map_err(|e| {
        DaemonizeError::OutputFileError(format!("cannot open {}: {e}", path.display()))
    })?;
    let st = nix::sys::stat::fstat(&fd)
        .map_err(|e| DaemonizeError::OutputFileError(format!("fstat {}: {e}", path.display())))?;
    Ok(OpenStream {
        path,
        id: FileId::of(&st),
        // A FIFO or a device (`-o /dev/null`) has no length to truncate:
        // `ftruncate` refuses one, where `O_TRUNC` used to ignore it.
        regular: st.st_mode & libc::S_IFMT == libc::S_IFREG,
        fd,
    })
}

/// Empties a stream's file, as `O_TRUNC` would have when it was opened.
fn truncate(stream: &OpenStream) -> Result<(), DaemonizeError> {
    if !stream.regular {
        return Ok(());
    }
    #[cfg(test)]
    if failpoints::injected(&failpoints::STREAM_TRUNCATE_FAILS) {
        return Err(DaemonizeError::OutputFileError(
            "truncate: injected failure".into(),
        ));
    }
    nix::unistd::ftruncate(&stream.fd, 0).map_err(|e| {
        DaemonizeError::OutputFileError(format!("truncate {}: {e}", stream.path.display()))
    })
}

/// Step 12: Redirect stdout/stderr to configured files.
///
/// Runs before the caller's split-phase privilege drop (R102), so files are
/// created with the original (often root) ownership;
/// [`drop_privileges`](crate::DaemonContext::drop_privileges) chowns them to
/// the target user afterward.
///
/// Each stream is opened without truncating it and compared by identity with
/// `owned` — the files the sequence holds, by the name to report them under
/// (R146). stdout is on fd 1 before stderr is opened, so a stderr of
/// `/dev/stdout` names the stdout file and shares its descriptor.
///
/// With [`Rollback::PutBack`], a failure undoes what this did (R147). Each
/// stdio slot is saved before it moves and put back if anything after fails;
/// the truncations, which cannot be undone, come last. A stream file it created
/// stays, empty, and an I/O error truncating stderr after stdout's truncation
/// succeeded leaves stdout emptied.
pub(crate) fn redirect_output(
    stdout: Option<&Path>,
    stderr: Option<&Path>,
    append: bool,
    owned: &[(&'static str, FileId)],
    rollback: Rollback,
) -> Result<(), DaemonizeError> {
    #[cfg(test)]
    crate::test_support::assert_isolated("redirect_output");
    let mut moved = Vec::new();

    let out = match stdout {
        Some(path) => {
            let out = open_stream(path, append)?;
            plan_output_redirect(Some(out.id), None, owned).map_err(|c| refusal(&c, &out))?;
            move_slot(&out.fd, StdioSlot::Stdout, rollback, &mut moved)?;
            Some(out)
        }
        None => None,
    };

    let err = match stderr {
        Some(path) => {
            let err = open_checked_stderr(path, append, out.as_ref(), owned)?;
            let source = match &err {
                Stderr::Own(err) => &err.fd,
                Stderr::SharesStdout(out) => &out.fd,
            };
            move_slot(source, StdioSlot::Stderr, rollback, &mut moved)?;
            Some(err)
        }
        None => None,
    };

    if !append {
        if let Some(out) = &out {
            truncate(out)?;
        }
        if let Some(Stderr::Own(err)) = &err {
            truncate(err)?;
        }
    }
    moved.into_iter().for_each(SavedSlot::keep);
    Ok(())
}

/// Whether a failed step 12 puts the stdio slots it moved back (R147).
#[derive(Clone, Copy)]
pub(crate) enum Rollback {
    /// Save each slot before moving it, and restore it on failure. Saving
    /// needs a free descriptor, so this fails when none is left.
    PutBack,
    /// Move the slots without saving them. For a daemon child, which exits on
    /// any failure, so no one sees the slots afterward.
    Skip,
}

/// A stdio slot step 12 moves a stream onto.
#[derive(Clone, Copy)]
enum StdioSlot {
    Stdout,
    Stderr,
}

impl StdioSlot {
    fn fd(self) -> i32 {
        match self {
            Self::Stdout => 1,
            Self::Stderr => 2,
        }
    }

    /// A copy of what the slot holds; `EBADF` if it is closed.
    fn save(self) -> std::io::Result<OwnedFd> {
        match self {
            Self::Stdout => std::io::stdout().as_fd().try_clone_to_owned(),
            Self::Stderr => std::io::stderr().as_fd().try_clone_to_owned(),
        }
    }

    #[allow(clippy::disallowed_methods)] // the stdio mover redirect_output uses
    fn dup2(self, source: impl AsFd) -> Result<(), nix::errno::Errno> {
        #[cfg(test)]
        crate::test_support::assert_isolated("StdioSlot::dup2");
        match self {
            Self::Stdout => unistd::dup2_stdout(source),
            Self::Stderr => unistd::dup2_stderr(source),
        }
    }
}

/// Moves `source` onto stdio slot `target`, saved in `moved` when `rollback`
/// asks for it. The choice lives here, not in [`SavedSlot`], so a
/// `SavedSlot` always holds what it saved.
fn move_slot(
    source: &OwnedFd,
    target: StdioSlot,
    rollback: Rollback,
    moved: &mut Vec<SavedSlot>,
) -> Result<(), DaemonizeError> {
    match rollback {
        Rollback::PutBack => moved.push(SavedSlot::redirect(source, target)?),
        Rollback::Skip => dup2_slot(source, target)?,
    }
    Ok(())
}

/// Moves `source` onto stdio slot `target`.
fn dup2_slot(source: &OwnedFd, target: StdioSlot) -> Result<(), DaemonizeError> {
    target
        .dup2(source)
        .map_err(|e| DaemonizeError::OutputFileError(format!("dup2 fd {}: {e}", target.fd())))
}

/// stderr as step 12 opened it.
enum Stderr<'a, 'o> {
    /// A file of its own.
    Own(OpenStream<'a>),
    /// The stdout file, which stderr then writes through stdout's descriptor,
    /// sharing its offset.
    SharesStdout(&'o OpenStream<'a>),
}

/// Opens stderr and compares it, with stdout already on fd 1.
fn open_checked_stderr<'a, 'o>(
    path: &'a Path,
    append: bool,
    out: Option<&'o OpenStream<'a>>,
    owned: &[(&'static str, FileId)],
) -> Result<Stderr<'a, 'o>, DaemonizeError> {
    let err = open_stream(path, append)?;
    let plan = plan_output_redirect(out.map(|o| o.id), Some(err.id), owned)
        .map_err(|c| refusal(&c, &err))?;
    Ok(match out.filter(|_| plan.stderr_shares_stdout) {
        Some(out) => Stderr::SharesStdout(out),
        None => Stderr::Own(err),
    })
}

/// A stdio slot step 12 has moved, put back as it was when this is dropped —
/// on any failure after the move — unless [`keep`](Self::keep) is called.
struct SavedSlot {
    target: StdioSlot,
    /// What the slot held, or `None` if it was closed.
    was: Option<OwnedFd>,
    restore: bool,
}

impl SavedSlot {
    /// Saves `target` and moves `source` onto it. Fails before the move if
    /// the slot cannot be saved, so nothing has changed then.
    fn redirect(source: &OwnedFd, target: StdioSlot) -> Result<Self, DaemonizeError> {
        let was = match target.save() {
            Ok(fd) => Some(fd),
            Err(e) if e.raw_os_error() == Some(libc::EBADF) => None,
            Err(e) => {
                return Err(DaemonizeError::OutputFileError(format!(
                    "cannot save fd {}: {e}",
                    target.fd()
                )));
            }
        };
        dup2_slot(source, target)?;
        Ok(Self {
            target,
            was,
            restore: true,
        })
    }

    /// Leaves the slot where it was moved.
    fn keep(mut self) {
        self.restore = false;
    }
}

impl Drop for SavedSlot {
    fn drop(&mut self) {
        if !self.restore {
            return;
        }
        match &self.was {
            Some(fd) => {
                let _ = self.target.dup2(fd);
            }
            None => crate::unsafe_ops::raw_close(self.target.fd()),
        }
    }
}

/// The refusal for a stream that turned out to be an owned file.
fn refusal(collision: &Collision, stream: &OpenStream) -> DaemonizeError {
    DaemonizeError::ValidationError(format!(
        "{} and {} must not be the same file: {}",
        collision.owned,
        collision.stream,
        stream.path.display()
    ))
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

/// Convert an `rlim_cur` value to the exclusive upper bound of the fd-close
/// range, saturating at `i32::MAX`.
///
/// A plain `as i32` cast wraps huge limits — `RLIM_INFINITY` becomes `-1`,
/// emptying the close range so *no* fds are closed. Saturating is lossless in
/// practice: fds are C ints, so no open fd can exceed `i32::MAX`.
///
/// `rlim_t` is unsigned on Linux/macOS/NetBSD but signed (`i64`) on FreeBSD,
/// so the conversion goes through `try_from`: any value outside `i32` range —
/// including a negative one, which no kernel should report — maps to
/// `i32::MAX`, erring toward closing everything rather than nothing.
pub(crate) fn clamp_max_fd(rlim_cur: libc::rlim_t) -> i32 {
    i32::try_from(rlim_cur).unwrap_or(i32::MAX)
}

/// Query the process fd limit via getrlimit.
///
/// Returns [`SystemError`](DaemonizeError::SystemError) if `getrlimit` fails
/// (e.g. blocked by a seccomp filter) so the caller can report it.
pub(crate) fn get_max_fd() -> Result<i32, DaemonizeError> {
    #[cfg(test)]
    if failpoints::injected(&failpoints::GETRLIMIT_FAILS) {
        return Err(DaemonizeError::SystemError(
            "getrlimit(RLIMIT_NOFILE): injected failure".into(),
        ));
    }
    #[cfg(test)]
    if let Some(max_fd) = failpoints::injected_max_fd() {
        return Ok(max_fd);
    }
    let limit = nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE)
        .map_err(|e| DaemonizeError::SystemError(format!("getrlimit(RLIMIT_NOFILE): {e}")))?;
    Ok(clamp_max_fd(limit.0))
}

/// Return an iterator of fd numbers to close, filtering out skip_fds.
///
/// Pure logic — no side effects.
pub(crate) fn fds_to_close(max_fd: i32, skip_fds: &[i32]) -> impl Iterator<Item = i32> + '_ {
    (3..max_fd).filter(move |fd| !skip_fds.contains(fd))
}

/// List this process's open fds from the fd directory, or `None` where no
/// reliable listing exists: the BSDs' `/dev/fd` exposes only 0-2 unless
/// fdescfs is mounted, and a minimal container may lack `/proc` even where the
/// platform normally has it (read failure also returns `None`).
///
/// The listing includes the fd `read_dir` itself uses; it is already closed
/// when the caller acts on the list, and re-closing is a harmless `EBADF`.
///
/// Safe post-fork: `daemonize` requires a single-threaded caller, so the
/// child's allocator lock cannot be held mid-operation by another thread.
#[cfg(blivet_fd_dir)]
pub(crate) fn list_open_fds() -> Option<Vec<i32>> {
    const FD_LIST_DIR: &str = env!("BLIVET_FD_DIR");

    #[cfg(test)]
    if failpoints::injected(&failpoints::FD_LISTING_UNAVAILABLE) {
        return None;
    }
    let entries = std::fs::read_dir(FD_LIST_DIR).ok()?;
    Some(
        entries
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .collect(),
    )
}

#[cfg(not(blivet_fd_dir))]
#[cfg_attr(test, mutants::skip)]
pub(crate) fn list_open_fds() -> Option<Vec<i32>> {
    None
}

/// Step 13: Close inherited file descriptors.
///
/// Closes the fds named by [`list_open_fds`] (minus 0-2 and the skip list),
/// falling back to iterating 3..rlim_cur where no listing is available.
/// The fallback matters for speed, not just portability: `RLIMIT_NOFILE` is
/// commonly raised to 1M+ (systemd `LimitNOFILE`) and `RLIM_INFINITY` clamps
/// to `i32::MAX`, turning the brute-force loop into billions of `close`
/// calls that stall daemon startup.
///
/// Returns [`SystemError`](DaemonizeError::SystemError) if the fallback path's
/// `getrlimit` fails; the fd-listing path is infallible.
pub(crate) fn close_inherited_fds(skip_fds: &[i32]) -> Result<(), DaemonizeError> {
    #[cfg(test)]
    crate::test_support::assert_isolated("close_inherited_fds");
    if let Some(open_fds) = list_open_fds() {
        for fd in open_fds {
            if fd >= 3 && !skip_fds.contains(&fd) {
                unsafe_ops::raw_close(fd);
            }
        }
        return Ok(());
    }
    let max_fd = get_max_fd()?;
    for fd in fds_to_close(max_fd, skip_fds) {
        unsafe_ops::raw_close(fd);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use super::*;
    use crate::test_support::{is_subprocess, run_in_subprocess};

    // --- Step 4: umask ---

    #[test]
    fn set_umask_applies_and_can_be_read_back() {
        crate::test_support::run_in_subprocess(
            "steps::tests::set_umask_applies_and_can_be_read_back_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn set_umask_applies_and_can_be_read_back_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let old = nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
        set_umask(0o022);
        let readback = nix::sys::stat::umask(old); // restore
        assert_eq!(readback, Mode::from_bits_truncate(0o022));
        nix::sys::stat::umask(old); // double-restore
    }

    // --- Step 5: chdir ---

    #[test]
    fn change_dir_to_tempdir() {
        crate::test_support::run_in_subprocess("steps::tests::change_dir_to_tempdir_subprocess");
    }

    #[test]
    #[ignore]
    fn change_dir_to_tempdir_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let original = std::env::current_dir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let result = change_dir(tmp.path());
        assert!(result.is_ok());
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(cwd, std::fs::canonicalize(tmp.path()).unwrap());
        std::env::set_current_dir(&original).unwrap();
    }

    #[test]
    fn change_dir_nonexistent_fails() {
        crate::test_support::run_in_subprocess(
            "steps::tests::change_dir_nonexistent_fails_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn change_dir_nonexistent_fails_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let result = change_dir(Path::new("/nonexistent_daemonize_test_path"));
        assert!(matches!(result, Err(DaemonizeError::ChdirFailed(_))));
    }

    // --- Step 6: redirect to /dev/null ---

    // Covers: R7, R8
    #[test]
    fn redirect_to_devnull_succeeds() {
        run_in_subprocess("steps::tests::redirect_to_devnull_succeeds_subprocess");
    }

    #[test]
    #[ignore]
    fn redirect_to_devnull_succeeds_subprocess() {
        use nix::sys::stat::fstat;

        if !is_subprocess() {
            return;
        }
        let devnull = fstat(open(c"/dev/null", OFlag::O_RDONLY, Mode::empty()).unwrap()).unwrap();

        redirect_to_devnull(true).unwrap();

        // fd 2 is /dev/null too by now, so a failure here reports only through
        // the exit status.
        for (name, slot) in [
            ("stdin", fstat(std::io::stdin()).unwrap()),
            ("stdout", fstat(std::io::stdout()).unwrap()),
            ("stderr", fstat(std::io::stderr()).unwrap()),
        ] {
            assert_eq!(
                (slot.st_dev, slot.st_ino),
                (devnull.st_dev, devnull.st_ino),
                "{name} is not /dev/null"
            );
        }
    }

    /// With stdin closed, `open("/dev/null")` hands back fd 0 itself. The
    /// redirect must still leave fd 0 open on /dev/null afterwards.
    #[test]
    fn redirect_to_devnull_into_a_closed_stdin_slot() {
        run_in_subprocess("steps::tests::redirect_to_devnull_into_a_closed_stdin_slot_subprocess");
    }

    #[test]
    #[ignore]
    fn redirect_to_devnull_into_a_closed_stdin_slot_subprocess() {
        use nix::sys::stat::fstat;

        if !is_subprocess() {
            return;
        }
        nix::unistd::close(0).unwrap();

        redirect_to_devnull(false).unwrap();

        let devnull = fstat(open(c"/dev/null", OFlag::O_RDONLY, Mode::empty()).unwrap()).unwrap();
        let stdin = fstat(std::io::stdin()).expect("fd 0 is not open after the redirect");
        assert_eq!(
            (stdin.st_dev, stdin.st_ino),
            (devnull.st_dev, devnull.st_ino)
        );
    }

    // Covers: R7
    #[test]
    fn redirect_to_devnull_foreground_preserves_stdout_stderr() {
        run_in_subprocess(
            "steps::tests::redirect_to_devnull_foreground_preserves_stdout_stderr_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn redirect_to_devnull_foreground_preserves_stdout_stderr_subprocess() {
        if !is_subprocess() {
            return;
        }
        use nix::sys::stat::fstat;

        let stdout_before = fstat(std::io::stdout()).unwrap();
        let stderr_before = fstat(std::io::stderr()).unwrap();
        redirect_to_devnull(false).unwrap();

        // stdin should be /dev/null
        let devnull = fstat(open(c"/dev/null", OFlag::O_RDONLY, Mode::empty()).unwrap()).unwrap();
        let stdin_after = fstat(std::io::stdin()).unwrap();
        assert_eq!(stdin_after.st_dev, devnull.st_dev);
        assert_eq!(stdin_after.st_ino, devnull.st_ino);

        // stdout and stderr should still point to the same files as before
        let stdout_after = fstat(std::io::stdout()).unwrap();
        let stderr_after = fstat(std::io::stderr()).unwrap();
        assert_eq!(stdout_before.st_dev, stdout_after.st_dev);
        assert_eq!(stdout_before.st_ino, stdout_after.st_ino);
        assert_eq!(stderr_before.st_dev, stderr_after.st_dev);
        assert_eq!(stderr_before.st_ino, stderr_after.st_ino);
    }

    // --- Step 7: open and lock ---

    // Covers: R96
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

    // --- Step 8: write pidfile ---

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
        run_in_subprocess("steps::tests::write_pidfile_standalone_mode_is_0644_subprocess");
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
        set_umask(0);
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

    // --- Step 9: signal disposition reset ---

    // Covers: R99
    #[test]
    fn reset_signal_dispositions_restores_default() {
        crate::test_support::run_in_subprocess(
            "steps::tests::reset_signal_dispositions_restores_default_subprocess",
        );
    }

    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn reset_signal_dispositions_restores_default_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
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
        crate::unsafe_ops::reset_signal_dispositions().unwrap();

        // Read back SIGUSR1 disposition — should be SIG_DFL now
        let after_reset = unsafe { sigaction(Signal::SIGUSR1, &old) }.unwrap();
        assert!(
            matches!(after_reset.handler(), SigHandler::SigDfl),
            "SIGUSR1 should be SIG_DFL after reset"
        );

        // Restore original
        let _ = unsafe { sigaction(Signal::SIGUSR1, &old) };
    }

    // Covers: R127
    #[test]
    fn reset_signal_dispositions_preserves_sigpipe() {
        crate::test_support::run_in_subprocess(
            "steps::tests::reset_signal_dispositions_preserves_sigpipe_subprocess",
        );
    }

    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn reset_signal_dispositions_preserves_sigpipe_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

        // Read the harness's current SIGPIPE disposition so it can be
        // restored at the end (install-and-put-back is the only read API).
        let probe = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        let original = unsafe { sigaction(Signal::SIGPIPE, &probe) }.unwrap();
        let _ = unsafe { sigaction(Signal::SIGPIPE, &original) };

        // SIG_IGN (what the Rust runtime installs) must survive the reset:
        // resetting it would turn every write to a closed pipe/socket into
        // silent process death instead of an EPIPE error.
        let ign = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        unsafe { sigaction(Signal::SIGPIPE, &ign) }.unwrap();
        crate::unsafe_ops::reset_signal_dispositions().unwrap();
        let after = unsafe { sigaction(Signal::SIGPIPE, &ign) }.unwrap();
        assert!(
            matches!(after.handler(), SigHandler::SigIgn),
            "SIGPIPE SIG_IGN must survive the disposition reset"
        );

        // The reset preserves the caller's choice, whatever it is — it does
        // not force SIG_IGN: an explicit SIG_DFL stays SIG_DFL.
        let dfl = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        unsafe { sigaction(Signal::SIGPIPE, &dfl) }.unwrap();
        crate::unsafe_ops::reset_signal_dispositions().unwrap();
        let after = unsafe { sigaction(Signal::SIGPIPE, &dfl) }.unwrap();
        assert!(
            matches!(after.handler(), SigHandler::SigDfl),
            "an explicit SIG_DFL must also be preserved, not overridden"
        );

        let _ = unsafe { sigaction(Signal::SIGPIPE, &original) };
    }

    // --- Step 10: clear signal mask ---

    // sigprocmask changes only the calling thread's mask, so this runs in the
    // shared test process.
    #[test]
    fn clear_signal_mask_empties() {
        use nix::sys::signal::{SigSet, SigmaskHow, Signal};
        // Block SIGUSR1
        let mut set = SigSet::empty();
        set.add(Signal::SIGUSR1);
        nix::sys::signal::sigprocmask(SigmaskHow::SIG_BLOCK, Some(&set), None).unwrap();

        clear_signal_mask().unwrap();

        let mut current = SigSet::empty();
        nix::sys::signal::sigprocmask(SigmaskHow::SIG_SETMASK, None, Some(&mut current)).unwrap();
        assert!(!current.contains(Signal::SIGUSR1));
    }

    // --- Step 11: set env vars ---

    #[test]
    fn set_env_vars_applies() {
        crate::test_support::run_in_subprocess("steps::tests::set_env_vars_applies_subprocess");
    }

    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn set_env_vars_applies_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let vars = vec![
            ("DAEMONIZE_TEST_A".into(), "1".into()),
            ("DAEMONIZE_TEST_B".into(), "2".into()),
        ];
        set_env_vars(&vars);
        assert_eq!(std::env::var("DAEMONIZE_TEST_A").unwrap(), "1");
        assert_eq!(std::env::var("DAEMONIZE_TEST_B").unwrap(), "2");
    }

    #[test]
    fn set_env_vars_last_write_wins() {
        crate::test_support::run_in_subprocess(
            "steps::tests::set_env_vars_last_write_wins_subprocess",
        );
    }

    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn set_env_vars_last_write_wins_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let vars = vec![
            ("DAEMONIZE_TEST_DUP".into(), "first".into()),
            ("DAEMONIZE_TEST_DUP".into(), "second".into()),
        ];
        set_env_vars(&vars);
        assert_eq!(std::env::var("DAEMONIZE_TEST_DUP").unwrap(), "second");
    }

    // --- Step 12: redirect output (pure plan tests) ---

    fn id(ino: u64) -> FileId {
        FileId {
            dev: 1,
            ino: ino as libc::ino_t,
        }
    }

    #[test]
    fn plan_distinct_streams_get_a_descriptor_each() {
        let plan = plan_output_redirect(Some(id(1)), Some(id(2)), &[]);
        assert_eq!(
            plan,
            Ok(RedirectPlan {
                stderr_shares_stdout: false
            })
        );
    }

    // Covers: R146
    #[test]
    fn plan_streams_on_one_file_share_a_descriptor() {
        let plan = plan_output_redirect(Some(id(1)), Some(id(1)), &[]);
        assert_eq!(
            plan,
            Ok(RedirectPlan {
                stderr_shares_stdout: true
            })
        );
    }

    #[test]
    fn plan_stderr_alone_shares_nothing() {
        let plan = plan_output_redirect(None, Some(id(1)), &[]);
        assert_eq!(
            plan,
            Ok(RedirectPlan {
                stderr_shares_stdout: false
            })
        );
    }

    // Covers: R146
    #[test]
    fn plan_refuses_a_stream_that_is_an_owned_file() {
        let owned = [("pidfile", id(7)), ("lockfile", id(8))];
        assert_eq!(
            plan_output_redirect(Some(id(1)), Some(id(8)), &owned),
            Err(Collision {
                stream: "stderr",
                owned: "lockfile"
            })
        );
        assert_eq!(
            plan_output_redirect(Some(id(7)), None, &owned),
            Err(Collision {
                stream: "stdout",
                owned: "pidfile"
            })
        );
    }

    #[test]
    fn plan_same_inode_on_another_device_is_another_file() {
        let other = FileId { dev: 2, ino: 1 };
        assert_eq!(
            plan_output_redirect(Some(id(1)), Some(other), &[]),
            Ok(RedirectPlan {
                stderr_shares_stdout: false
            })
        );
    }

    /// Outside a subprocess the stdio movers refuse to run. No paths, so the
    /// call would move nothing even if the refusal were gone.
    #[test]
    #[should_panic(expected = "run_in_subprocess")]
    fn redirect_output_refuses_the_shared_test_process() {
        let _ = redirect_output(None, None, false, &[], Rollback::PutBack);
    }

    // --- Step 12: redirect output (executor smoke tests) ---
    //
    // Each body moves fd 1 or 2, so each runs in its own process: see
    // crate::test_support::assert_isolated.

    // Covers: R98
    #[test]
    fn execute_redirect_creates_files() {
        run_in_subprocess("steps::tests::execute_redirect_creates_files_subprocess");
    }

    #[test]
    #[ignore]
    fn execute_redirect_creates_files_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        let stderr_path = dir.path().join("stderr.log");
        let result = redirect_output(
            Some(&stdout_path),
            Some(&stderr_path),
            false,
            &[],
            Rollback::PutBack,
        );
        assert!(result.is_ok());
        assert!(stdout_path.exists());
        assert!(stderr_path.exists());
    }

    #[test]
    fn execute_redirect_truncate_vs_append() {
        run_in_subprocess("steps::tests::execute_redirect_truncate_vs_append_subprocess");
    }

    #[test]
    #[ignore]
    fn execute_redirect_truncate_vs_append_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");

        // Truncate mode. The old content is longer than the new, so a skipped
        // truncate leaves a tail behind instead of being overwritten exactly.
        std::fs::write(&stdout_path, "old content, longer than what replaces it\n").unwrap();
        redirect_output(Some(&stdout_path), None, false, &[], Rollback::PutBack).unwrap();
        std::io::stdout().write_all(b"new content\n").unwrap();
        std::io::stdout().flush().unwrap();
        // A skipped truncate would leave the old line's tail behind.
        assert_eq!(
            std::fs::read_to_string(&stdout_path).unwrap(),
            "new content\n"
        );

        // Append mode
        redirect_output(Some(&stdout_path), None, true, &[], Rollback::PutBack).unwrap();
        std::io::stdout().write_all(b"appended\n").unwrap();
        std::io::stdout().flush().unwrap();
        assert_eq!(
            std::fs::read_to_string(&stdout_path).unwrap(),
            "new content\nappended\n"
        );
    }

    #[test]
    fn execute_redirect_dup_stdout_to_stderr() {
        run_in_subprocess("steps::tests::execute_redirect_dup_stdout_to_stderr_subprocess");
    }

    #[test]
    #[ignore]
    fn execute_redirect_dup_stdout_to_stderr_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let combined = dir.path().join("combined.log");
        redirect_output(
            Some(&combined),
            Some(&combined),
            false,
            &[],
            Rollback::PutBack,
        )
        .unwrap();

        std::io::stdout().write_all(b"stdout\n").unwrap();
        std::io::stdout().flush().unwrap();
        std::io::stderr().write_all(b"stderr\n").unwrap();
        std::io::stderr().flush().unwrap();

        // One descriptor means one offset, so stderr's line follows stdout's;
        // two would put both at offset 0 and one would overwrite the other.
        assert_eq!(
            std::fs::read_to_string(&combined).unwrap(),
            "stdout\nstderr\n"
        );
    }

    // Covers: R147
    #[test]
    fn execute_redirect_that_fails_on_stderr_changes_nothing() {
        run_in_subprocess(
            "steps::tests::execute_redirect_that_fails_on_stderr_changes_nothing_subprocess",
        );
    }

    /// A start that fails must not have emptied the previous run's log, nor
    /// left the caller's stdout pointing somewhere else. A directory as stderr
    /// fails to open for root too, so every tier sees the failure.
    #[test]
    #[ignore]
    fn execute_redirect_that_fails_on_stderr_changes_nothing_subprocess() {
        use nix::sys::stat::fstat;

        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "the previous run\n").unwrap();
        let stdout_before = fstat(std::io::stdout()).unwrap();

        let result = redirect_output(Some(&log), Some(dir.path()), false, &[], Rollback::PutBack);

        assert!(
            matches!(result, Err(DaemonizeError::OutputFileError(_))),
            "{result:?}"
        );
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "the previous run\n");
        let stdout_after = fstat(std::io::stdout()).unwrap();
        assert_eq!(
            (stdout_before.st_dev, stdout_before.st_ino),
            (stdout_after.st_dev, stdout_after.st_ino),
            "a failed redirect left fd 1 redirected"
        );
    }

    // Covers: R147
    #[test]
    fn execute_redirect_that_fails_with_stdout_closed_leaves_it_closed() {
        run_in_subprocess(
            "steps::tests::execute_redirect_that_fails_with_stdout_closed_leaves_it_closed_subprocess",
        );
    }

    /// A caller with fd 1 closed must find it closed after a failed start, not
    /// open on the log at offset 0, where its next write would overwrite the
    /// start of the previous run's output.
    #[test]
    #[ignore]
    fn execute_redirect_that_fails_with_stdout_closed_leaves_it_closed_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "the previous run\n").unwrap();
        nix::unistd::close(1).unwrap();

        let result = redirect_output(Some(&log), Some(dir.path()), false, &[], Rollback::PutBack);

        assert!(
            matches!(result, Err(DaemonizeError::OutputFileError(_))),
            "{result:?}"
        );
        assert_eq!(
            nix::fcntl::fcntl(std::io::stdout(), nix::fcntl::FcntlArg::F_GETFD),
            Err(nix::errno::Errno::EBADF),
            "a failed redirect left fd 1 open"
        );
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "the previous run\n");
    }

    // Covers: R147
    #[test]
    fn execute_redirect_that_fails_with_stderr_closed_leaves_it_closed() {
        run_in_subprocess(
            "steps::tests::execute_redirect_that_fails_with_stderr_closed_leaves_it_closed_subprocess",
        );
    }

    /// The fd 2 counterpart: a failure after stderr moved must close fd 2
    /// again, not fd 1.
    #[test]
    #[ignore]
    fn execute_redirect_that_fails_with_stderr_closed_leaves_it_closed_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.err");
        std::fs::write(&log, "the previous run\n").unwrap();
        nix::unistd::close(2).unwrap();
        failpoints::STREAM_TRUNCATE_FAILS.store(true, std::sync::atomic::Ordering::Relaxed);

        let result = redirect_output(None, Some(&log), false, &[], Rollback::PutBack);

        assert!(
            matches!(result, Err(DaemonizeError::OutputFileError(_))),
            "{result:?}"
        );
        assert_eq!(
            nix::fcntl::fcntl(std::io::stderr(), nix::fcntl::FcntlArg::F_GETFD),
            Err(nix::errno::Errno::EBADF),
            "a failed redirect left fd 2 open"
        );
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "the previous run\n");
    }

    // Covers: R147
    #[test]
    fn execute_redirect_that_cannot_save_stdout_changes_nothing() {
        run_in_subprocess(
            "steps::tests::execute_redirect_that_cannot_save_stdout_changes_nothing_subprocess",
        );
    }

    /// With no descriptor left to save fd 1 in, the redirect must fail before
    /// moving it, not treat fd 1 as closed and carry on.
    #[test]
    #[ignore]
    fn execute_redirect_that_cannot_save_stdout_changes_nothing_subprocess() {
        use nix::sys::stat::fstat;

        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "the previous run\n").unwrap();
        let stdout_before = fstat(std::io::stdout()).unwrap();

        // Fill every descriptor under a small limit, then free one: the stdout
        // file takes it, and saving fd 1 finds none.
        let held = crate::test_support::fill_fd_table(1);

        let result = redirect_output(Some(&log), None, false, &[], Rollback::PutBack);
        drop(held);

        assert!(
            matches!(&result, Err(DaemonizeError::OutputFileError(m)) if m.contains("fd 1")),
            "{result:?}"
        );
        let stdout_after = fstat(std::io::stdout()).unwrap();
        assert_eq!(
            (stdout_before.st_dev, stdout_before.st_ino),
            (stdout_after.st_dev, stdout_after.st_ino),
            "a failed redirect left fd 1 redirected"
        );
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "the previous run\n");
    }

    // Covers: R147
    #[test]
    fn execute_redirect_that_fails_late_puts_both_slots_back() {
        run_in_subprocess(
            "steps::tests::execute_redirect_that_fails_late_puts_both_slots_back_subprocess",
        );
    }

    /// A failure after both slots have moved — here, the truncation, the last
    /// step — must put fd 1 and fd 2 back as they were.
    #[test]
    #[ignore]
    fn execute_redirect_that_fails_late_puts_both_slots_back_subprocess() {
        use nix::sys::stat::fstat;

        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("app.log");
        let err = dir.path().join("app.err");
        std::fs::write(&out, "previous out\n").unwrap();
        std::fs::write(&err, "previous err\n").unwrap();
        let id = |st: nix::sys::stat::FileStat| (st.st_dev, st.st_ino);
        let before = (
            id(fstat(std::io::stdout()).unwrap()),
            id(fstat(std::io::stderr()).unwrap()),
        );
        failpoints::STREAM_TRUNCATE_FAILS.store(true, std::sync::atomic::Ordering::Relaxed);

        let result = redirect_output(Some(&out), Some(&err), false, &[], Rollback::PutBack);

        let after = (
            id(fstat(std::io::stdout()).unwrap()),
            id(fstat(std::io::stderr()).unwrap()),
        );
        assert!(
            matches!(result, Err(DaemonizeError::OutputFileError(_))),
            "{result:?}"
        );
        assert_eq!(before, after, "a failed redirect left fd 1 or fd 2 moved");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "previous out\n");
        assert_eq!(std::fs::read_to_string(&err).unwrap(), "previous err\n");
    }

    // Covers: R146
    #[test]
    fn execute_redirect_stderr_onto_dev_stdout_follows_the_redirected_stdout() {
        run_in_subprocess("steps::tests::execute_redirect_stderr_onto_dev_stdout_follows_the_redirected_stdout_subprocess");
    }

    #[test]
    #[ignore]
    fn execute_redirect_stderr_onto_dev_stdout_follows_the_redirected_stdout_subprocess() {
        if !is_subprocess() {
            return;
        }
        // `-e /dev/stdout` names whatever fd 1 is when stderr is opened, so
        // stderr must be opened after stdout has moved onto fd 1 — then it is
        // the stdout file, and shares its descriptor.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("combined.log");

        redirect_output(
            Some(&log),
            Some(Path::new("/dev/stdout")),
            false,
            &[],
            Rollback::PutBack,
        )
        .unwrap();
        std::io::stdout().write_all(b"stdout\n").unwrap();
        std::io::stdout().flush().unwrap();
        std::io::stderr().write_all(b"stderr\n").unwrap();
        std::io::stderr().flush().unwrap();

        let content = std::fs::read_to_string(&log).unwrap();
        let out = content.find("stdout\n");
        let err = content.find("stderr\n");
        assert!(
            matches!((out, err), (Some(o), Some(e)) if o < e),
            "stderr should land in the stdout file, after stdout's line: {content:?}"
        );
    }

    #[test]
    fn execute_redirect_stderr_only() {
        run_in_subprocess("steps::tests::execute_redirect_stderr_only_subprocess");
    }

    #[test]
    #[ignore]
    fn execute_redirect_stderr_only_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stderr_path = dir.path().join("stderr.log");
        redirect_output(None, Some(&stderr_path), false, &[], Rollback::PutBack).unwrap();
        assert!(stderr_path.exists());

        std::io::stderr().write_all(b"stderr content\n").unwrap();
        std::io::stderr().flush().unwrap();
        let content = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(content.contains("stderr content"));
    }

    #[test]
    fn execute_redirect_truncates_a_separate_stderr_file() {
        run_in_subprocess(
            "steps::tests::execute_redirect_truncates_a_separate_stderr_file_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn execute_redirect_truncates_a_separate_stderr_file_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        let stderr_path = dir.path().join("stderr.log");
        std::fs::write(&stderr_path, "stale stderr from a previous run\n").unwrap();

        redirect_output(
            Some(&stdout_path),
            Some(&stderr_path),
            false,
            &[],
            Rollback::PutBack,
        )
        .unwrap();

        let content = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(
            !content.contains("stale stderr"),
            "should have truncated: {content:?}"
        );
    }

    /// With stdout closed, `open` hands back fd 1 itself. The redirect must
    /// still leave fd 1 pointing at the file afterwards, not closed.
    #[test]
    fn execute_redirect_into_a_closed_stdout_slot() {
        run_in_subprocess("steps::tests::execute_redirect_into_a_closed_stdout_slot_subprocess");
    }

    #[test]
    #[ignore]
    fn execute_redirect_into_a_closed_stdout_slot_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        nix::unistd::close(1).unwrap();

        redirect_output(Some(&stdout_path), None, false, &[], Rollback::PutBack).unwrap();

        let written = nix::unistd::write(std::io::stdout(), b"reached\n");
        assert_eq!(written, Ok(8), "fd 1 is not open after the redirect");
        assert_eq!(std::fs::read_to_string(&stdout_path).unwrap(), "reached\n");
    }

    /// The same for fd 2, the highest slot `open_above_stdio` must keep a
    /// source out of: with stderr closed, the redirect must leave fd 2 open on
    /// the file.
    #[test]
    fn execute_redirect_into_a_closed_stderr_slot() {
        run_in_subprocess("steps::tests::execute_redirect_into_a_closed_stderr_slot_subprocess");
    }

    #[test]
    #[ignore]
    fn execute_redirect_into_a_closed_stderr_slot_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stderr_path = dir.path().join("stderr.log");
        nix::unistd::close(2).unwrap();

        redirect_output(None, Some(&stderr_path), false, &[], Rollback::PutBack).unwrap();

        let written = nix::unistd::write(std::io::stderr(), b"reached\n");
        assert_eq!(written, Ok(8), "fd 2 is not open after the redirect");
        assert_eq!(std::fs::read_to_string(&stderr_path).unwrap(), "reached\n");
    }

    // Covers: R146
    #[test]
    fn execute_redirect_refuses_an_owned_file_before_truncating_it() {
        run_in_subprocess(
            "steps::tests::execute_redirect_refuses_an_owned_file_before_truncating_it_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn execute_redirect_refuses_an_owned_file_before_truncating_it_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("daemon.pid");
        std::fs::write(&pidfile, "4242\n").unwrap();
        let owned = [(
            "pidfile",
            file_id(std::fs::File::open(&pidfile).unwrap()).unwrap(),
        )];

        let result = redirect_output(Some(&pidfile), None, false, &owned, Rollback::PutBack);

        let named = pidfile.display().to_string();
        assert!(
            matches!(&result, Err(DaemonizeError::ValidationError(m))
                if m.contains("same file") && m.contains(&named)),
            "the refusal should name the stream's file: {result:?}"
        );
        assert_eq!(std::fs::read_to_string(&pidfile).unwrap(), "4242\n");
    }

    #[test]
    fn execute_redirect_into_a_device_does_not_truncate_it() {
        run_in_subprocess(
            "steps::tests::execute_redirect_into_a_device_does_not_truncate_it_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn execute_redirect_into_a_device_does_not_truncate_it_subprocess() {
        if !is_subprocess() {
            return;
        }
        // ftruncate refuses a device where O_TRUNC ignored it; `-o /dev/null`
        // must keep working.
        redirect_output(
            Some(Path::new("/dev/null")),
            None,
            false,
            &[],
            Rollback::PutBack,
        )
        .unwrap();
    }

    // --- Step 13: close inherited fds (pure plan tests) ---

    // Covers: R103
    #[test]
    fn clamp_max_fd_saturates_instead_of_wrapping() {
        // RLIM_INFINITY formerly wrapped to -1 via `as i32`, emptying the
        // close range so no fds were closed at all. The conversion must
        // saturate: closing up to i32::MAX covers every possible fd (fds are
        // C ints). rlim_t::MAX covers both the unsigned (u64::MAX, which IS
        // RLIM_INFINITY on Linux) and signed-FreeBSD (i64::MAX) cases.
        assert_eq!(clamp_max_fd(libc::rlim_t::MAX), i32::MAX);
        assert_eq!(clamp_max_fd(i32::MAX as libc::rlim_t + 1), i32::MAX);
        // Ordinary limits pass through unchanged.
        assert_eq!(clamp_max_fd(1024), 1024);
        assert_eq!(clamp_max_fd(0), 0);
    }

    // Covers: R104
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

    // Covers: R103
    #[test]
    fn get_max_fd_reflects_a_real_limit() {
        // 0-2 always exist, so any true fd limit is at least 3. Guards the
        // fallback close range against collapsing (0/1) or inverting (-1).
        assert!(get_max_fd().unwrap() >= 3);
    }

    // Covers: R135
    #[test]
    #[cfg(blivet_fd_dir)]
    fn list_open_fds_sees_an_open_fd() {
        use std::os::fd::AsRawFd;
        let file = tempfile::tempfile().unwrap();
        let listed = list_open_fds().expect("fd listing is available on this platform");
        assert!(
            listed.contains(&file.as_raw_fd()),
            "an open fd must appear in the listing"
        );
        // No assertion that a *closed* fd disappears: the listing itself
        // opens the fd directory, which can reuse the just-freed number.
    }

    // --- Step 13: close inherited fds (executor smoke test, subprocess-isolated) ---
    //
    // close_inherited_fds closes every non-skipped descriptor process-wide, so
    // the body runs in its own process: see crate::test_support::assert_isolated.

    /// Runs everywhere, the hosted Ubuntu runner included, so mutation testing
    /// reaches step 13 in CI. It was once ignored there after an abort blamed
    /// on systemd's `safe_close()` (7dc982b, eb50694); a hosted run with the
    /// ignore removed passed, so the ignore went. If that abort comes back,
    /// the ignore is not the fix: find what in the test process owns the fd.
    /// `close_inherited_fds_without_a_listing_preserves_skipped` forces the
    /// brute-force branch wherever it runs, so no tier is needed for that.
    // Covers: R103, R104
    #[test]
    fn close_inherited_fds_preserves_skipped() {
        crate::test_support::run_in_subprocess(
            "steps::tests::close_inherited_fds_preserves_skipped_subprocess",
        );
    }

    #[test]
    #[ignore = "closes fds process-wide; only safe in an isolated subprocess"]
    fn close_inherited_fds_preserves_skipped_subprocess() {
        // Guard so this never runs as a stray `--include-ignored` in the shared
        // process; it executes only when spawned by run_in_subprocess.
        if !crate::test_support::is_subprocess() {
            return;
        }
        assert_closes_all_but_skipped();
    }

    /// The same property, proven on the brute-force `3..rlim_cur` fallback.
    ///
    /// That branch is what every platform without a trustworthy fd directory
    /// takes at runtime — the BSDs, and every best-effort Unix — and the only
    /// other test that reaches it fails `getrlimit` as well, so it returns
    /// before closing anything. Forcing the listing to be unavailable runs the
    /// fallback on whichever tier runs this test, rather than waiting for a
    /// tier that lacks the directory.
    // Covers: R103, R104, R135
    #[test]
    fn close_inherited_fds_without_a_listing_preserves_skipped() {
        crate::test_support::run_in_subprocess(
            "steps::tests::close_inherited_fds_without_a_listing_preserves_skipped_subprocess",
        );
    }

    #[test]
    #[ignore = "closes fds process-wide; only safe in an isolated subprocess"]
    fn close_inherited_fds_without_a_listing_preserves_skipped_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        // Process-global, which is why this runs isolated. Where the platform
        // has no fd directory the flag changes nothing and the fallback was
        // already the only branch.
        failpoints::FD_LISTING_UNAVAILABLE.store(true, std::sync::atomic::Ordering::Relaxed);
        // Bounded, so the loop is the branch under test rather than the host's
        // fd limit. Comfortably above the handful of descriptors a test process
        // holds, and far below what a raised RLIMIT_NOFILE would make it walk.
        failpoints::MAX_FD.store(4096, std::sync::atomic::Ordering::Relaxed);
        assert_closes_all_but_skipped();
    }

    /// Closes every fd it was not told to keep, and keeps the ones it was.
    ///
    /// Closes fds process-wide, so only an isolated subprocess may call it.
    fn assert_closes_all_but_skipped() {
        let (rd, wr) = crate::test_support::make_pipe();
        // A second pipe deliberately left out of the skip list: it must be
        // closed, or the step silently no-oped (a mutation sweep caught the
        // original test asserting only preservation, never closure).
        let (victim_rd, victim_wr) = crate::test_support::make_pipe();
        drop(victim_rd);
        close_inherited_fds(&[rd.as_raw_fd(), wr.as_raw_fd()]).unwrap();
        // Our pipe fds should still be open
        assert!(nix::unistd::write(&wr, b"ok").is_ok());
        // The non-skipped fd must be gone.
        assert_eq!(
            nix::unistd::write(&victim_wr, b"x"),
            Err(nix::errno::Errno::EBADF),
            "a non-skipped fd must be closed"
        );
        // close_inherited_fds already closed it; don't double-close on drop.
        std::mem::forget(victim_wr);
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
