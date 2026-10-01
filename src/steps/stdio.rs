//! Standard streams: `/dev/null` on stdio (step 6), redirecting stdout and
//! stderr to the configured files (step 12), and keeping every descriptor
//! the sequence opens above fd 2 (R152).

use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;

use nix::fcntl::{open, OFlag};
use nix::sys::stat::Mode;
use nix::unistd::{self};

use super::*;
use crate::error::DaemonizeError;
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

/// Opens `path`, never on fd 0, 1 or 2 (see [`above_stdio`]). Every file the
/// sequence opens goes through here; clippy.toml bans a bare
/// `nix::fcntl::open`.
#[allow(clippy::disallowed_methods)] // the one place a bare open is allowed
pub(super) fn open_above_stdio<P: ?Sized + nix::NixPath>(
    path: &P,
    flags: OFlag,
    mode: Mode,
) -> std::io::Result<OwnedFd> {
    above_stdio(open(path, flags, mode)?)
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
    if crate::steps::failpoints::DEVNULL_OPEN_FAILS.injected() {
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
    if crate::steps::failpoints::STREAM_TRUNCATE_FAILS.injected() {
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
/// Runs before the identity switch (R102), so files are created with the
/// original (often root) ownership; the sequence chowns them to the target
/// user just before switching.
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
            None => crate::sys::raw_close(self.target.fd()),
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

/// `fd`, moved above descriptor 2 if it sits on a stdio slot (R152).
///
/// With a stdio slot closed, `open` and `pipe` hand out that slot. A later
/// `dup2` onto it, step 6's or step 12's, would then close the descriptor:
/// a lock dropped, a pipe end lost, a redirect source its own target.
/// `try_clone` duplicates close-on-exec with a floor of 3, and the original
/// closes on drop. A descriptor already above 2 is returned as is, so a table
/// with only the descriptors it needs free still succeeds.
pub(crate) fn above_stdio(fd: OwnedFd) -> std::io::Result<OwnedFd> {
    if fd.as_raw_fd() > 2 {
        Ok(fd)
    } else {
        fd.try_clone()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    use crate::test_support::{is_subprocess, run_in_subprocess};

    // Covers: R7, R8
    #[test]
    fn redirect_to_devnull_succeeds() {
        run_in_subprocess("steps::stdio::tests::redirect_to_devnull_succeeds_subprocess");
    }

    #[test]
    #[ignore]
    #[allow(clippy::disallowed_methods)] // a test probe; no stdio slot is in play
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
        run_in_subprocess(
            "steps::stdio::tests::redirect_to_devnull_into_a_closed_stdin_slot_subprocess",
        );
    }

    #[test]
    #[ignore]
    #[allow(clippy::disallowed_methods)] // a test probe; no stdio slot is in play
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
            "steps::stdio::tests::redirect_to_devnull_foreground_preserves_stdout_stderr_subprocess",
        );
    }

    #[test]
    #[ignore]
    #[allow(clippy::disallowed_methods)] // a test probe; no stdio slot is in play
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

    // Covers: R98
    #[test]
    fn execute_redirect_creates_files() {
        run_in_subprocess("steps::stdio::tests::execute_redirect_creates_files_subprocess");
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
        run_in_subprocess("steps::stdio::tests::execute_redirect_truncate_vs_append_subprocess");
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
        run_in_subprocess("steps::stdio::tests::execute_redirect_dup_stdout_to_stderr_subprocess");
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
            "steps::stdio::tests::execute_redirect_that_fails_on_stderr_changes_nothing_subprocess",
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
            "steps::stdio::tests::execute_redirect_that_fails_with_stdout_closed_leaves_it_closed_subprocess",
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
            "steps::stdio::tests::execute_redirect_that_fails_with_stderr_closed_leaves_it_closed_subprocess",
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
        crate::steps::failpoints::STREAM_TRUNCATE_FAILS.set();

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
            "steps::stdio::tests::execute_redirect_that_cannot_save_stdout_changes_nothing_subprocess",
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
            "steps::stdio::tests::execute_redirect_that_fails_late_puts_both_slots_back_subprocess",
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
        crate::steps::failpoints::STREAM_TRUNCATE_FAILS.set();

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
        run_in_subprocess("steps::stdio::tests::execute_redirect_stderr_onto_dev_stdout_follows_the_redirected_stdout_subprocess");
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
        run_in_subprocess("steps::stdio::tests::execute_redirect_stderr_only_subprocess");
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
            "steps::stdio::tests::execute_redirect_truncates_a_separate_stderr_file_subprocess",
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
        run_in_subprocess(
            "steps::stdio::tests::execute_redirect_into_a_closed_stdout_slot_subprocess",
        );
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
        run_in_subprocess(
            "steps::stdio::tests::execute_redirect_into_a_closed_stderr_slot_subprocess",
        );
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
            "steps::stdio::tests::execute_redirect_refuses_an_owned_file_before_truncating_it_subprocess",
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
            "steps::stdio::tests::execute_redirect_into_a_device_does_not_truncate_it_subprocess",
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
}
