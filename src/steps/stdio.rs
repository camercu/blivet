//! Standard streams: `/dev/null` on stdio (step 6), redirecting stdout and
//! stderr to the configured files (step 12), and keeping every descriptor
//! the sequence opens above fd 2 (R152).

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use nix::fcntl::{OFlag, open};
use nix::sys::stat::Mode;
use nix::unistd::{self};

use super::{Ledger, Rollback, Saved, Undo};
use crate::error::{ChangedState, ErrorKind, RestoreFailure};
use crate::file_identity::{FileId, is_symlink};
use std::os::fd::AsRawFd;

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

/// Opens `path` close-on-exec, never on fd 0, 1 or 2 (see [`above_stdio`]).
/// Every file the sequence opens and holds goes through here; clippy.toml
/// bans the other ways to open one.
#[allow(clippy::disallowed_methods)] // the one place a bare open is allowed
pub(crate) fn open_above_stdio<P: ?Sized + nix::NixPath>(
    path: &P,
    flags: OFlag,
    mode: Mode,
) -> std::io::Result<OwnedFd> {
    above_stdio(open(path, flags | OFlag::O_CLOEXEC, mode)?)
}

/// [`open_above_stdio`] with `O_CREAT`, saying whether this open created the
/// file, which a failed start then removes (R164). A file that appears
/// between the two opens counts as found, and stays. One that goes between
/// them, as another instance's rollback removes it, is tried again: the
/// file this start then creates is its own.
pub(crate) fn open_creating(
    path: &Path,
    flags: OFlag,
    mode: Mode,
) -> std::io::Result<(OwnedFd, bool)> {
    for _ in 0..OPEN_ATTEMPTS {
        let exclusive = match open_above_stdio(path, flags | OFlag::O_CREAT | OFlag::O_EXCL, mode) {
            Ok(fd) => return Ok((fd, true)),
            Err(e) => e,
        };
        #[cfg(test)]
        if crate::steps::failpoints::UNLINK_BETWEEN_OPENS.injected() {
            let _ = std::fs::remove_file(path);
        }
        // Without `O_CREAT`, so this open cannot create the file and call it
        // found.
        match open_above_stdio(path, flags, mode) {
            Ok(fd) => return Ok((fd, false)),
            Err(e) if e.raw_os_error() != Some(libc::ENOENT) => return Err(e),
            Err(_) => {}
        }
        // A symlink is `EEXIST` to the exclusive open, dangling or not.
        // Dangling, the file it names is created through it, and is not this
        // start's to remove: the path is the link.
        if is_symlink(path) {
            return open_above_stdio(path, flags | OFlag::O_CREAT, mode).map(|fd| (fd, false));
        }
        // Not there for either open: the exclusive open's error is the cause.
        if exclusive.raw_os_error() != Some(libc::EEXIST) {
            return Err(exclusive);
        }
    }
    Err(std::io::Error::other(format!(
        "{} kept appearing and going",
        path.display()
    )))
}

/// How often [`open_creating`] tries again when the path went between its
/// two opens. Each retry means another instance removed the file in that
/// moment, so a few suffice.
const OPEN_ATTEMPTS: usize = 8;

/// Step 6: Redirect standard streams to /dev/null.
///
/// Always redirects stdin. When `stdout_stderr` is true, also redirects
/// stdout and stderr. In foreground mode, stdout/stderr are left
/// inherited so output reaches the parent terminal or supervisor.
///
/// Returns [`SystemError`](ErrorKind::SystemError) if `/dev/null` cannot
/// be opened or a `dup2` fails (e.g. a minimal container with no `/dev/null`),
/// so the caller can report the failure to the parent rather than crashing.
/// Each slot's token goes in `ledger` as the slot moves, so a failure part
/// way leaves the slots already moved for the ledger to put back.
#[allow(clippy::disallowed_methods)] // a stdio mover; asserts isolation under test
pub(crate) fn redirect_to_devnull(
    stdout_stderr: bool,
    ledger: &mut Ledger,
) -> Result<(), ErrorKind> {
    #[cfg(test)]
    crate::test_support::assert_isolated("redirect_to_devnull");
    #[cfg(test)]
    if crate::steps::failpoints::DEVNULL_OPEN_FAILS.injected() {
        return Err(ErrorKind::SystemError(
            "open /dev/null: injected failure".into(),
        ));
    }
    let devnull = open_above_stdio(c"/dev/null", OFlag::O_RDWR, Mode::empty())
        .map_err(|e| ErrorKind::SystemError(format!("open /dev/null: {e}")))?;
    let slots: &[StdioSlot] = if stdout_stderr {
        &[StdioSlot::Stdin, StdioSlot::Stdout, StdioSlot::Stderr]
    } else {
        &[StdioSlot::Stdin]
    };
    for &slot in slots {
        let undo =
            move_slot(&devnull, slot, ledger.rollback()).map_err(|e| e.into_devnull_error(slot))?;
        ledger.record(undo)?;
    }
    Ok(())
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

/// A stream's file, opened but not yet redirected.
struct OpenStream<'a> {
    path: &'a Path,
    fd: OwnedFd,
    id: FileId,
}

/// Opens `path` for a stream without truncating it: nothing on disk may
/// change until [`plan_output_redirect`] has seen what the file is. A file it
/// creates gets a token in `ledger` that removes it again.
fn open_stream<'a>(
    path: &'a Path,
    append: bool,
    ledger: &mut Ledger,
) -> Result<OpenStream<'a>, ErrorKind> {
    let mut flags = OFlag::O_WRONLY;
    if append {
        flags |= OFlag::O_APPEND;
    }
    let (fd, created) = open_creating(path, flags, Mode::from_bits_truncate(0o644))
        .map_err(|e| ErrorKind::OutputFileError(format!("cannot open {}: {e}", path.display())))?;
    let st = nix::sys::stat::fstat(&fd)
        .map_err(|e| ErrorKind::OutputFileError(format!("fstat {}: {e}", path.display())))?;
    let id = FileId::of(&st);
    if created {
        let removal = super::Removal::new(path, id, super::Created::Log);
        ledger.record_lasting(Saved::Removal(removal))?;
    }
    Ok(OpenStream { path, id, fd })
}

/// Step 15's first act: empties each stream file step 12 opened, as
/// `O_TRUNC` would have, unless appending (R11).
///
/// The truncation cannot be undone, so it waits until just before `init`: a
/// start that fails before it leaves the previous run's logs as they were.
/// A file it empties of content gets a token in either mode that reports the
/// loss if the start then fails (R164). It goes in `ledger` as the file
/// empties, so a failure part way reports the files already emptied.
pub(crate) fn truncate_streams<'p>(
    files: impl IntoIterator<Item = (&'p Path, BorrowedFd<'p>)>,
    ledger: &mut Ledger,
) -> Result<(), ErrorKind> {
    for (path, fd) in files {
        let failed =
            |what, e| ErrorKind::OutputFileError(format!("{what} {}: {e}", path.display()));
        let st = nix::sys::stat::fstat(fd).map_err(|e| failed("fstat", e))?;
        // A FIFO or a device (`-o /dev/null`) has no length to truncate:
        // `ftruncate` refuses one, where `O_TRUNC` used to ignore it.
        if st.st_mode & libc::S_IFMT != libc::S_IFREG {
            continue;
        }
        #[cfg(test)]
        if crate::steps::failpoints::STREAM_TRUNCATE_FAILS.injected() {
            return Err(ErrorKind::OutputFileError(
                "truncate: injected failure".into(),
            ));
        }
        nix::unistd::ftruncate(fd, 0).map_err(|e| failed("truncate", e))?;
        if st.st_size > 0 {
            ledger.record_lasting(Saved::Emptied(Emptied(path.to_path_buf())))?;
        }
    }
    Ok(())
}

/// A stream file step 15 emptied of content, which no rollback can put back.
pub(crate) struct Emptied(std::path::PathBuf);

impl Emptied {
    /// Reports the content lost.
    pub(crate) fn restore(self) -> Result<(), RestoreFailure> {
        Err(RestoreFailure::at(
            ChangedState::EmptiedOutputFile,
            &self.0,
            std::io::Error::other("truncated, so its earlier content is lost"),
        ))
    }
}

/// Step 12: Redirect stdout/stderr to configured files.
///
/// Runs before the identity switch (R102), so files are created with the
/// original (often root) ownership; the sequence chowns them to the target
/// user just before switching.
///
/// Each stream is opened without truncating it and compared by identity with
/// `owned` — the files the sequence holds, by the name to report them under
/// (R146). stdout is on fd 1 before stderr is opened, so a stderr of
/// `/dev/stdout` names the stdout file and shares its descriptor.
///
/// Each change's token goes in `ledger` as it is made: a moved slot's under
/// [`Rollback::PutBack`] (R147, R164), a created stream file's in either
/// mode, which removes it on failure while nothing has written to it. A
/// failure part way leaves them for the ledger to put back. Each stdio slot
/// is saved before it moves.
///
/// Returns each stream's file by the path that names it, once: stderr
/// sharing stdout's file is not repeated, with its identity. Close-on-exec,
/// above fd 2. Step 15 truncates them and step 16 chowns them, by these
/// descriptors, not by whatever `init` left on fds 1 and 2 (R163). This
/// truncates nothing: see [`truncate_streams`].
pub(crate) fn redirect_output<'a>(
    stdout: Option<&'a Path>,
    stderr: Option<&'a Path>,
    append: bool,
    owned: &[(&'static str, FileId)],
    ledger: &mut Ledger,
) -> Result<Vec<(&'a Path, OwnedFd, FileId)>, ErrorKind> {
    #[cfg(test)]
    crate::test_support::assert_isolated("redirect_output");
    let out = match stdout {
        Some(path) => {
            let out = open_stream(path, append, ledger)?;
            plan_output_redirect(Some(out.id), None, owned).map_err(|c| refusal(&c, &out))?;
            let undo = move_slot(&out.fd, StdioSlot::Stdout, ledger.rollback())
                .map_err(|e| e.into_output_error(StdioSlot::Stdout))?;
            ledger.record(undo)?;
            Some(out)
        }
        None => None,
    };

    let err = match stderr {
        Some(path) => {
            let err = open_checked_stderr(path, append, out.as_ref(), owned, ledger)?;
            let source = match &err {
                Stderr::Own(err) => &err.fd,
                Stderr::SharesStdout(out) => &out.fd,
            };
            let undo = move_slot(source, StdioSlot::Stderr, ledger.rollback())
                .map_err(|e| e.into_output_error(StdioSlot::Stderr))?;
            ledger.record(undo)?;
            Some(err)
        }
        None => None,
    };

    let files = err
        .and_then(|err| match err {
            Stderr::Own(err) => Some((err.path, err.fd, err.id)),
            Stderr::SharesStdout(_) => None,
        })
        .into_iter()
        .chain(out.map(|out| (out.path, out.fd, out.id)))
        .collect();
    Ok(files)
}

/// A stdio slot steps 6 and 12 move a file onto.
#[derive(Clone, Copy, Debug)]
enum StdioSlot {
    Stdin,
    Stdout,
    Stderr,
}

impl StdioSlot {
    /// The state this slot is, as a rollback failure names it.
    fn state(self) -> ChangedState {
        match self {
            Self::Stdin => ChangedState::Stdin,
            Self::Stdout => ChangedState::Stdout,
            Self::Stderr => ChangedState::Stderr,
        }
    }

    fn fd(self) -> i32 {
        match self {
            Self::Stdin => 0,
            Self::Stdout => 1,
            Self::Stderr => 2,
        }
    }

    /// A copy of what the slot holds; `EBADF` if it is closed.
    fn save(self) -> std::io::Result<OwnedFd> {
        match self {
            Self::Stdin => std::io::stdin().as_fd().try_clone_to_owned(),
            Self::Stdout => std::io::stdout().as_fd().try_clone_to_owned(),
            Self::Stderr => std::io::stderr().as_fd().try_clone_to_owned(),
        }
    }

    #[allow(clippy::disallowed_methods)] // the one stdio mover
    fn dup2(self, source: impl AsFd) -> Result<(), nix::errno::Errno> {
        #[cfg(test)]
        crate::test_support::assert_isolated("StdioSlot::dup2");
        match self {
            Self::Stdin => unistd::dup2_stdin(source),
            Self::Stdout => unistd::dup2_stdout(source),
            Self::Stderr => unistd::dup2_stderr(source),
        }
    }
}

/// Why a stdio slot could not be moved: saving it, or the `dup2`.
pub(crate) enum SlotError {
    Save(std::io::Error),
    Dup2(nix::errno::Errno),
}

impl SlotError {
    /// Step 6's error: moving `/dev/null` onto a slot is a system failure.
    fn into_devnull_error(self, slot: StdioSlot) -> ErrorKind {
        ErrorKind::SystemError(match self {
            Self::Save(e) => format!("/dev/null -> fd {}: cannot save it: {e}", slot.fd()),
            Self::Dup2(e) => format!("dup2 /dev/null -> fd {}: {e}", slot.fd()),
        })
    }

    /// Step 12's error: moving a stream onto a slot is an output-file failure.
    fn into_output_error(self, slot: StdioSlot) -> ErrorKind {
        ErrorKind::OutputFileError(match self {
            Self::Save(e) => format!("cannot save fd {}: {e}", slot.fd()),
            Self::Dup2(e) => format!("dup2 fd {}: {e}", slot.fd()),
        })
    }
}

/// Moves `source` onto stdio slot `target`, returning its token, which
/// restores nothing unless `rollback` asks for one. Saving a slot needs a
/// free descriptor, so under [`Rollback::PutBack`] this fails when none is
/// left. The choice lives here, not in [`SavedSlot`], so a `SavedSlot` always
/// holds what it saved.
fn move_slot(source: &OwnedFd, target: StdioSlot, rollback: Rollback) -> Result<Undo, SlotError> {
    match rollback {
        Rollback::PutBack => Ok(Undo::new(Saved::Slot(SavedSlot::redirect(source, target)?))),
        Rollback::Skip => {
            target.dup2(source).map_err(SlotError::Dup2)?;
            Ok(Undo::nothing())
        }
    }
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
    ledger: &mut Ledger,
) -> Result<Stderr<'a, 'o>, ErrorKind> {
    let err = open_stream(path, append, ledger)?;
    let plan = plan_output_redirect(out.map(|o| o.id), Some(err.id), owned)
        .map_err(|c| refusal(&c, &err))?;
    Ok(match out.filter(|_| plan.stderr_shares_stdout) {
        Some(out) => Stderr::SharesStdout(out),
        None => Stderr::Own(err),
    })
}

/// A stdio slot a step has moved, and what it held before.
pub(crate) struct SavedSlot {
    target: StdioSlot,
    /// What the slot held, or `None` if it was closed.
    was: Option<OwnedFd>,
}

impl SavedSlot {
    /// Saves `target` and moves `source` onto it. Fails before the move if
    /// the slot cannot be saved, so nothing has changed then.
    fn redirect(source: &OwnedFd, target: StdioSlot) -> Result<Self, SlotError> {
        let was = match target.save() {
            Ok(fd) => Some(fd),
            Err(e) if e.raw_os_error() == Some(libc::EBADF) => None,
            Err(e) => return Err(SlotError::Save(e)),
        };
        target.dup2(source).map_err(SlotError::Dup2)?;
        Ok(Self { target, was })
    }

    pub(crate) fn restore(self) -> Result<(), RestoreFailure> {
        match &self.was {
            Some(fd) => self
                .target
                .dup2(fd)
                .map_err(|e| RestoreFailure::new(self.target.state(), e)),
            None => {
                crate::sys::raw_close(self.target.fd());
                Ok(())
            }
        }
    }

    /// The saved copy, which the fd sweep must spare.
    pub(crate) fn held_fd(&self) -> Option<i32> {
        self.was.as_ref().map(AsRawFd::as_raw_fd)
    }
}

/// The refusal for a stream that turned out to be an owned file.
fn refusal(collision: &Collision, stream: &OpenStream) -> ErrorKind {
    ErrorKind::ValidationError(format!(
        "{} and {} must not be the same file: {}",
        collision.owned,
        collision.stream,
        stream.path.display()
    ))
}

/// `fd`, moved above descriptor 2 if it sits on a stdio slot (R152).
///
/// With a stdio slot closed, `open` and `socketpair` hand out that slot. A
/// later `dup2` onto it, step 6's or step 12's, would then close the
/// descriptor: a lock dropped, a channel end lost, a redirect source its own
/// target. `try_clone` duplicates close-on-exec with a floor of 3, and the
/// original closes on drop. A descriptor already above 2 is returned as is, so
/// a table with only the descriptors it needs free still succeeds.
pub(crate) fn above_stdio(fd: OwnedFd) -> std::io::Result<OwnedFd> {
    if fd.as_raw_fd() > 2 {
        Ok(fd)
    } else {
        fd.try_clone()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_file_removed_between_the_opens_is_created_by_this_start() {
        if !crate::test_support::isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.lock");
        std::fs::write(&path, "").unwrap();
        crate::steps::failpoints::UNLINK_BETWEEN_OPENS.set();

        let (_fd, created) =
            open_creating(&path, OFlag::O_WRONLY, Mode::from_bits_truncate(0o644)).unwrap();

        assert!(created);
    }

    #[test]
    fn a_dangling_symlink_creates_its_target_as_found() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.log");
        let link = dir.path().join("app.log");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let (_fd, created) =
            open_creating(&link, OFlag::O_WRONLY, Mode::from_bits_truncate(0o644)).unwrap();

        assert!(!created && target.exists());
    }

    #[test]
    fn a_file_neither_open_can_reach_reports_why() {
        if nix::unistd::geteuid().is_root() {
            return; // root writes to a read-only directory
        }
        let dir = tempfile::tempdir().unwrap();
        let sealed = dir.path().join("sealed");
        std::fs::create_dir(&sealed).unwrap();
        let mut perms = std::fs::metadata(&sealed).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o555);
        std::fs::set_permissions(&sealed, perms).unwrap();

        let err = open_creating(
            &sealed.join("app.log"),
            OFlag::O_WRONLY,
            Mode::from_bits_truncate(0o644),
        )
        .unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// A file there that this start may not open is reported as such, not
    /// taken for one that keeps appearing and going.
    #[test]
    fn a_file_found_but_not_writable_reports_why() {
        if nix::unistd::geteuid().is_root() {
            return; // root writes to a read-only file
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        std::fs::write(&path, "").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o444);
        std::fs::set_permissions(&path, perms).unwrap();

        let err =
            open_creating(&path, OFlag::O_WRONLY, Mode::from_bits_truncate(0o644)).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_file_found_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        std::fs::write(&path, "").unwrap();

        let (_fd, created) =
            open_creating(&path, OFlag::O_WRONLY, Mode::from_bits_truncate(0o644)).unwrap();

        assert!(!created);
    }

    /// `redirect_output` as a foreground sequence runs it: a failure puts
    /// back what it changed, and a success stays in place, as at step 18.
    fn kept(
        stdout: Option<&Path>,
        stderr: Option<&Path>,
        append: bool,
        owned: &[(&'static str, FileId)],
    ) -> Result<(), crate::ErrorKind> {
        let mut ledger = Ledger::new(Rollback::PutBack);
        redirect_output(stdout, stderr, append, owned, &mut ledger)?;
        ledger.keep_all();
        Ok(())
    }

    /// [`kept`], then step 15's truncation unless appending, as the sequence
    /// runs them.
    fn kept_truncated(
        stdout: Option<&Path>,
        stderr: Option<&Path>,
        append: bool,
    ) -> Result<(), crate::ErrorKind> {
        let mut ledger = Ledger::new(Rollback::PutBack);
        let files = redirect_output(stdout, stderr, append, &[], &mut ledger)?;
        if !append {
            let files: Vec<_> = files.iter().map(|(p, fd, _)| (*p, fd.as_fd())).collect();
            super::truncate_streams(files, &mut ledger)?;
        }
        ledger.keep_all();
        Ok(())
    }

    use crate::file_identity::file_id;
    use std::io::Write;

    use super::*;

    use crate::test_support::isolated;

    // Covers: R96, R168
    #[test]
    fn open_above_stdio_opens_close_on_exec_whatever_flags_the_caller_passes() {
        for passed in [OFlag::O_RDONLY, OFlag::O_RDONLY | OFlag::O_CLOEXEC] {
            let fd = open_above_stdio(c"/dev/null", passed, Mode::empty()).unwrap();

            let flags = nix::fcntl::fcntl(&fd, nix::fcntl::FcntlArg::F_GETFD).unwrap();

            assert!(
                nix::fcntl::FdFlag::from_bits_truncate(flags)
                    .contains(nix::fcntl::FdFlag::FD_CLOEXEC),
                "{passed:?}: a held file would leak into the daemon's program"
            );
        }
    }

    // Covers: R152, R168
    /// With stdin closed, the open lands on fd 0 and `above_stdio` moves it:
    /// the copy, not only the first open, must be close-on-exec.
    #[test]
    fn a_file_moved_off_a_stdio_slot_stays_close_on_exec() {
        use std::os::fd::AsRawFd;
        if !isolated() {
            return;
        }
        let fd = crate::test_support::with_fds_closed(&[0], || {
            open_above_stdio(c"/dev/null", OFlag::O_RDONLY, Mode::empty()).unwrap()
        });

        let flags = nix::fcntl::fcntl(&fd, nix::fcntl::FcntlArg::F_GETFD).unwrap();

        assert!(fd.as_raw_fd() > 2, "fd {}", fd.as_raw_fd());
        assert!(
            nix::fcntl::FdFlag::from_bits_truncate(flags).contains(nix::fcntl::FdFlag::FD_CLOEXEC),
            "the moved copy would leak into the daemon's program"
        );
    }

    // Covers: R152, R168
    /// With another stdio slot closed, the lowest free descriptor is that
    /// slot: no saved copy may land there, where step 6's `dup2` would
    /// replace it.
    #[test]
    fn a_saved_stdio_copy_sits_above_stdio_close_on_exec() {
        use std::os::fd::AsRawFd;
        if !isolated() {
            return;
        }
        for (slot, closed) in [
            (StdioSlot::Stdin, 1),
            (StdioSlot::Stdout, 0),
            (StdioSlot::Stderr, 0),
        ] {
            let saved = crate::test_support::with_fds_closed(&[closed], || slot.save().unwrap());

            let flags = nix::fcntl::fcntl(&saved, nix::fcntl::FcntlArg::F_GETFD).unwrap();

            assert!(
                saved.as_raw_fd() > 2,
                "slot {}: fd {}",
                slot.fd(),
                saved.as_raw_fd()
            );
            assert!(
                nix::fcntl::FdFlag::from_bits_truncate(flags)
                    .contains(nix::fcntl::FdFlag::FD_CLOEXEC),
                "slot {}: the saved copy would leak into the daemon's program",
                slot.fd()
            );
        }
    }

    // Covers: R7, R8
    #[test]
    #[allow(clippy::disallowed_methods)] // a test probe; no stdio slot is in play
    fn redirect_to_devnull_succeeds() {
        use nix::sys::stat::fstat;

        if !isolated() {
            return;
        }
        let devnull = fstat(open(c"/dev/null", OFlag::O_RDONLY, Mode::empty()).unwrap()).unwrap();

        redirect_to_devnull(true, &mut Ledger::new(Rollback::Skip)).unwrap();

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
    #[allow(clippy::disallowed_methods)] // a test probe; no stdio slot is in play
    fn redirect_to_devnull_into_a_closed_stdin_slot() {
        use nix::sys::stat::fstat;

        if !isolated() {
            return;
        }
        nix::unistd::close(0).unwrap();

        redirect_to_devnull(false, &mut Ledger::new(Rollback::Skip)).unwrap();

        let devnull = fstat(open(c"/dev/null", OFlag::O_RDONLY, Mode::empty()).unwrap()).unwrap();
        let stdin = fstat(std::io::stdin()).expect("fd 0 is not open after the redirect");
        assert_eq!(
            (stdin.st_dev, stdin.st_ino),
            (devnull.st_dev, devnull.st_ino)
        );
    }

    // Covers: R7
    #[test]
    #[allow(clippy::disallowed_methods)] // a test probe; no stdio slot is in play
    fn redirect_to_devnull_foreground_preserves_stdout_stderr() {
        if !isolated() {
            return;
        }
        use nix::sys::stat::fstat;

        let stdout_before = fstat(std::io::stdout()).unwrap();
        let stderr_before = fstat(std::io::stderr()).unwrap();
        redirect_to_devnull(false, &mut Ledger::new(Rollback::Skip)).unwrap();

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
    #[should_panic(expected = "changes process-wide state under the shared test process")]
    fn redirect_output_refuses_the_shared_test_process() {
        let _ = kept(None, None, false, &[]);
    }

    /// The one stdio mover refuses too. Stdin onto itself would move nothing
    /// even if the refusal were gone.
    #[test]
    #[should_panic(
        expected = "StdioSlot::dup2 changes process-wide state under the shared test process"
    )]
    fn the_stdio_mover_refuses_the_shared_test_process() {
        let _ = StdioSlot::Stdin.dup2(std::io::stdin());
    }

    // Covers: R98
    #[test]
    fn execute_redirect_creates_files() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        let stderr_path = dir.path().join("stderr.log");
        let result = kept(Some(&stdout_path), Some(&stderr_path), false, &[]);
        assert!(result.is_ok());
        assert!(stdout_path.exists());
        assert!(stderr_path.exists());
    }

    #[test]
    fn execute_redirect_truncate_vs_append() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");

        // Truncate mode. The old content is longer than the new, so a skipped
        // truncate leaves a tail behind instead of being overwritten exactly.
        std::fs::write(&stdout_path, "old content, longer than what replaces it\n").unwrap();
        kept_truncated(Some(&stdout_path), None, false).unwrap();
        std::io::stdout().write_all(b"new content\n").unwrap();
        std::io::stdout().flush().unwrap();
        // A skipped truncate would leave the old line's tail behind.
        assert_eq!(
            std::fs::read_to_string(&stdout_path).unwrap(),
            "new content\n"
        );

        // Append mode
        kept_truncated(Some(&stdout_path), None, true).unwrap();
        std::io::stdout().write_all(b"appended\n").unwrap();
        std::io::stdout().flush().unwrap();
        assert_eq!(
            std::fs::read_to_string(&stdout_path).unwrap(),
            "new content\nappended\n"
        );
    }

    #[test]
    fn execute_redirect_dup_stdout_to_stderr() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let combined = dir.path().join("combined.log");
        kept(Some(&combined), Some(&combined), false, &[]).unwrap();

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
    /// A start that fails must not have emptied the previous run's log, nor
    /// left the caller's stdout pointing somewhere else. A directory as stderr
    /// fails to open for root too, so every tier sees the failure.
    #[test]
    fn execute_redirect_that_fails_on_stderr_changes_nothing() {
        use nix::sys::stat::fstat;

        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "the previous run\n").unwrap();
        let stdout_before = fstat(std::io::stdout()).unwrap();

        let result = kept(Some(&log), Some(dir.path()), false, &[]);

        assert!(
            matches!(result, Err(ErrorKind::OutputFileError(_))),
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
    /// A caller with fd 1 closed must find it closed after a failed start, not
    /// open on the log at offset 0, where its next write would overwrite the
    /// start of the previous run's output.
    #[test]
    fn execute_redirect_that_fails_with_stdout_closed_leaves_it_closed() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "the previous run\n").unwrap();
        nix::unistd::close(1).unwrap();

        let result = kept(Some(&log), Some(dir.path()), false, &[]);

        assert!(
            matches!(result, Err(ErrorKind::OutputFileError(_))),
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
    /// With no descriptor left to save fd 1 in, the redirect must fail before
    /// moving it, not treat fd 1 as closed and carry on.
    #[test]
    fn execute_redirect_that_cannot_save_stdout_changes_nothing() {
        use nix::sys::stat::fstat;

        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "the previous run\n").unwrap();
        let stdout_before = fstat(std::io::stdout()).unwrap();

        // Fill every descriptor under a small limit, then free one: the stdout
        // file takes it, and saving fd 1 finds none.
        let held = crate::test_support::fill_fd_table(1);

        let result = kept(Some(&log), None, false, &[]);
        drop(held);

        assert!(
            matches!(&result, Err(ErrorKind::OutputFileError(m)) if m.contains("fd 1")),
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

    // Covers: R146
    #[test]
    fn execute_redirect_stderr_onto_dev_stdout_follows_the_redirected_stdout() {
        if !isolated() {
            return;
        }
        // `-e /dev/stdout` names whatever fd 1 is when stderr is opened, so
        // stderr must be opened after stdout has moved onto fd 1 — then it is
        // the stdout file, and shares its descriptor.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("combined.log");

        kept(Some(&log), Some(Path::new("/dev/stdout")), false, &[]).unwrap();
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
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stderr_path = dir.path().join("stderr.log");
        kept(None, Some(&stderr_path), false, &[]).unwrap();
        assert!(stderr_path.exists());

        std::io::stderr().write_all(b"stderr content\n").unwrap();
        std::io::stderr().flush().unwrap();
        let content = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(content.contains("stderr content"));
    }

    #[test]
    fn execute_redirect_truncates_a_separate_stderr_file() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        let stderr_path = dir.path().join("stderr.log");
        std::fs::write(&stderr_path, "stale stderr from a previous run\n").unwrap();

        kept_truncated(Some(&stdout_path), Some(&stderr_path), false).unwrap();

        let content = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(
            !content.contains("stale stderr"),
            "should have truncated: {content:?}"
        );
    }

    /// With stdout closed, `open` hands back fd 1 itself. The redirect must
    /// still leave fd 1 pointing at the file afterwards, not closed.
    #[test]
    #[allow(clippy::disallowed_methods)] // writes the redirect's file, not the channel
    fn execute_redirect_into_a_closed_stdout_slot() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.log");
        nix::unistd::close(1).unwrap();

        kept(Some(&stdout_path), None, false, &[]).unwrap();

        let written = nix::unistd::write(std::io::stdout(), b"reached\n");
        assert_eq!(written, Ok(8), "fd 1 is not open after the redirect");
        assert_eq!(std::fs::read_to_string(&stdout_path).unwrap(), "reached\n");
    }

    /// The same for fd 2, the highest slot `open_above_stdio` must keep a
    /// source out of: with stderr closed, the redirect must leave fd 2 open on
    /// the file.
    #[test]
    #[allow(clippy::disallowed_methods)] // writes the redirect's file, not the channel
    fn execute_redirect_into_a_closed_stderr_slot() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let stderr_path = dir.path().join("stderr.log");
        nix::unistd::close(2).unwrap();

        kept(None, Some(&stderr_path), false, &[]).unwrap();

        let written = nix::unistd::write(std::io::stderr(), b"reached\n");
        assert_eq!(written, Ok(8), "fd 2 is not open after the redirect");
        assert_eq!(std::fs::read_to_string(&stderr_path).unwrap(), "reached\n");
    }

    // Covers: R146
    #[test]
    #[allow(clippy::disallowed_methods)] // a test's own descriptor, not the sequence's
    fn execute_redirect_refuses_an_owned_file_before_truncating_it() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("daemon.pid");
        std::fs::write(&pidfile, "4242\n").unwrap();
        let owned = [(
            "pidfile",
            file_id(std::fs::File::open(&pidfile).unwrap()).unwrap(),
        )];

        let result = kept(Some(&pidfile), None, false, &owned);

        let named = pidfile.display().to_string();
        assert!(
            matches!(&result, Err(ErrorKind::ValidationError(m))
                if m.contains("same file") && m.contains(&named)),
            "the refusal should name the stream's file: {result:?}"
        );
        assert_eq!(std::fs::read_to_string(&pidfile).unwrap(), "4242\n");
    }

    #[test]
    fn execute_redirect_into_a_device_does_not_truncate_it() {
        if !isolated() {
            return;
        }
        // ftruncate refuses a device where O_TRUNC ignored it; `-o /dev/null`
        // must keep working.
        kept_truncated(Some(Path::new("/dev/null")), None, false).unwrap();
    }
}
