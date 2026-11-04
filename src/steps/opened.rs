//! The files the sequence opened at steps 7, 8 and 12: one record, which step
//! 12's collision check (R146), step 13's fd sweep (R152) and step 16's chown
//! (R163) all read.
//!
//! Each list was assembled by hand before, and a file added to one was missed
//! in the other. Both now derive from the record, and only the steps that
//! open files write it. stdio slots are never in it, so step 16 chowns the
//! files as opened, not whatever `init` left on fds 1 and 2.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use nix::fcntl::Flock;
use nix::unistd::{Gid, Uid};

use super::Ledger;
use crate::error::{ChangedState, ErrorKind, RestoreFailure};
use crate::file_identity::{FileId, lstat_if_named};

/// What a file is to the sequence, with the descriptor that holds it.
pub(crate) enum Role {
    /// Step 7's lock, held until the daemon ends.
    Lockfile(Flock<OwnedFd>),
    /// Step 8's pidfile, when it is not also the lockfile.
    Pidfile(OwnedFd),
    /// A step 12 stream file, once: stderr sharing stdout's is not repeated.
    Stream(OwnedFd),
}

impl AsRawFd for Role {
    fn as_raw_fd(&self) -> RawFd {
        self.as_fd().as_raw_fd()
    }
}

impl AsFd for Role {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Self::Lockfile(flock) => flock.as_fd(),
            Self::Pidfile(fd) | Self::Stream(fd) => fd.as_fd(),
        }
    }
}

/// What a start creates on disk, which a failure removes (R164).
#[derive(Clone, Copy)]
pub(crate) enum Created {
    Pidfile,
    Lockfile,
    /// A stdout or stderr file.
    Log,
}

impl Created {
    fn state(self) -> ChangedState {
        match self {
            Self::Pidfile => ChangedState::Pidfile,
            Self::Lockfile => ChangedState::Lockfile,
            Self::Log => ChangedState::OutputFile,
        }
    }

    /// Whether the file stays once written to. A log does: it may hold why
    /// the start failed.
    fn kept_once_written(self) -> bool {
        match self {
            Self::Log => true,
            Self::Pidfile | Self::Lockfile => false,
        }
    }
}

/// A file a step left on disk, to remove on failure in either mode (R164).
///
/// Removed only while the path itself is still the file: a file another
/// process put there since, or a symlink, even one to the file, is not this
/// one's to remove.
pub(crate) struct Removal {
    path: PathBuf,
    id: FileId,
    what: Created,
}

impl Removal {
    pub(crate) fn new(path: &Path, id: FileId, what: Created) -> Self {
        Self {
            path: path.to_path_buf(),
            id,
            what,
        }
    }

    pub(crate) fn restore(self) -> Result<(), RestoreFailure> {
        let failed = |e: std::io::Error| RestoreFailure::at(self.what.state(), &self.path, e);
        match lstat_if_named(&self.path, self.id) {
            Ok(Some(st)) if self.what.kept_once_written() && st.st_size > 0 => Ok(()),
            Ok(Some(_)) => std::fs::remove_file(&self.path).map_err(failed),
            Ok(None) => Ok(()),
            Err(e) => Err(failed(e.into())),
        }
    }
}

/// One opened file: the path it was opened by, its role, and its identity.
struct Opened<'a> {
    path: &'a Path,
    role: Role,
    id: FileId,
    /// The owner and group step 16 replaced, to give back by `role`'s
    /// descriptor on failure (R164).
    owner_was: Option<(Uid, Gid)>,
}

/// The files the sequence opened, in the order it opened them.
#[derive(Default)]
pub(crate) struct OpenedFiles<'a>(Vec<Opened<'a>>);

impl<'a> OpenedFiles<'a> {
    /// Records a file a step opened, with the identity `fstat` gave it.
    pub(crate) fn push(&mut self, path: &'a Path, role: Role, id: FileId) {
        self.0.push(Opened {
            path,
            role,
            id,
            owner_was: None,
        });
    }

    /// Step 12: the files a stream must not be (R146), by the name to report
    /// them under. The file with the `pidfile` identity is the pidfile even
    /// when step 7 opened it as the lock: the user configured it so.
    pub(crate) fn owned(&self, pidfile: Option<FileId>) -> Vec<(&'static str, FileId)> {
        self.0
            .iter()
            .filter_map(|o| {
                let name = match o.role {
                    _ if Some(o.id) == pidfile => "pidfile",
                    Role::Lockfile(_) => "lockfile",
                    Role::Pidfile(_) => "pidfile",
                    Role::Stream(_) => return None,
                };
                Some((name, o.id))
            })
            .collect()
    }

    /// Step 12's stream files, by the path each was opened by.
    pub(crate) fn streams(&self) -> Vec<(&'a Path, BorrowedFd<'_>)> {
        self.0
            .iter()
            .filter_map(|o| match &o.role {
                Role::Stream(fd) => Some((o.path, fd.as_fd())),
                Role::Lockfile(_) | Role::Pidfile(_) => None,
            })
            .collect()
    }

    /// The lock step 7 holds, with the path it was opened by.
    pub(crate) fn lock(&self) -> Option<(&'a Path, &Flock<OwnedFd>)> {
        self.0.iter().find_map(|o| match &o.role {
            Role::Lockfile(flock) => Some((o.path, flock)),
            Role::Pidfile(_) | Role::Stream(_) => None,
        })
    }

    /// Hands over the lock, for the context to hold. The other files close
    /// when the record drops.
    pub(crate) fn take_lock(&mut self) -> Option<Flock<OwnedFd>> {
        let at = self
            .0
            .iter()
            .position(|o| matches!(o.role, Role::Lockfile(_)))?;
        match self.0.remove(at).role {
            Role::Lockfile(flock) => Some(flock),
            Role::Pidfile(_) | Role::Stream(_) => None,
        }
    }
}

/// Step 13: the descriptors the sweep spares (R152). Every file in the
/// record, each descriptor an undo token holds, and the notification channel's
/// write end.
pub(crate) fn spared_fds(
    files: &OpenedFiles,
    held: impl IntoIterator<Item = RawFd>,
    pipe: Option<RawFd>,
) -> Vec<RawFd> {
    files
        .0
        .iter()
        .map(|o| o.role.as_raw_fd())
        .chain(held)
        .chain(pipe)
        .collect()
}

impl OpenedFiles<'_> {
    /// Step 16: chown each regular file in the record to `owner` and
    /// `group`, while the process is still privileged (R163).
    ///
    /// By descriptor, not path: the chown reaches the file the sequence
    /// opened, even if `init` has since changed what the path names, by a
    /// `chroot` or a rename. For stdout and stderr that is step 12's
    /// descriptor, as init may have moved fds 1 and 2. A symlink planted
    /// before the open still redirects the open itself, so these paths
    /// belong in directories untrusted users cannot write.
    ///
    /// Each file keeps the owner it had, in both modes: the owner is on disk,
    /// so it outlives a daemon that exits (R164).
    /// [`give_owners_back`](Self::give_owners_back) puts it back by the same
    /// descriptor. Each chown is a checkpoint of `ledger`'s, so the undo walk
    /// fails the step part way.
    ///
    /// # Errors
    ///
    /// Returns `ErrorKind::ChownError` naming the first file that fails.
    pub(crate) fn chown_to(
        &mut self,
        (owner, group): (Option<Uid>, Option<Gid>),
        ledger: &mut Ledger,
    ) -> Result<(), ErrorKind> {
        let failed = |path: &Path, e| ErrorKind::ChownError(format!("{}: {e}", path.display()));
        for o in &mut self.0 {
            let st = nix::sys::stat::fstat(o.role.as_fd()).map_err(|e| failed(o.path, e))?;
            // Only a regular file is the daemon's own. A device, FIFO, or
            // socket (`-o /dev/null`, a tty) belongs to the system; the
            // daemon writes through the descriptor it holds either way.
            if st.st_mode & libc::S_IFMT != libc::S_IFREG {
                continue;
            }
            nix::unistd::fchown(o.role.as_fd(), owner, group).map_err(|e| failed(o.path, e))?;
            o.owner_was = Some((Uid::from_raw(st.st_uid), Gid::from_raw(st.st_gid)));
            ledger.checkpoint()?;
        }
        Ok(())
    }

    /// Gives each file [`chown_to`](Self::chown_to) chowned its owner back,
    /// by the descriptor this record holds, and returns what it could not
    /// give back.
    pub(crate) fn give_owners_back(&mut self) -> Vec<RestoreFailure> {
        self.0
            .iter_mut()
            .filter_map(|o| {
                let (owner, group) = o.owner_was.take()?;
                nix::unistd::fchown(o.role.as_fd(), Some(owner), Some(group))
                    .err()
                    .map(|e| {
                        RestoreFailure::at(ChangedState::FileOwner, o.path, std::io::Error::from(e))
                    })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::path::Path;

    #[allow(clippy::disallowed_methods)] // a test's own descriptor, not the sequence's
    fn file(dir: &Path, name: &str) -> OwnedFd {
        std::fs::File::create(dir.join(name)).unwrap().into()
    }

    fn lock(dir: &Path, name: &str) -> nix::fcntl::Flock<OwnedFd> {
        crate::steps::open_and_lock(&dir.join(name)).unwrap()
    }

    /// Records `role` under `path`, by the identity its descriptor has.
    fn push<'a>(files: &mut OpenedFiles<'a>, path: &'a Path, role: Role) {
        let id = crate::file_identity::file_id(role.as_fd()).unwrap();
        files.push(path, role, id);
    }

    #[test]
    fn the_owned_files_are_named_by_role() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let lockfile = dir.path().join("app.lock");
        let pidfile = dir.path().join("app.pid");
        push(
            &mut files,
            &lockfile,
            Role::Lockfile(lock(dir.path(), "app.lock")),
        );
        push(
            &mut files,
            &pidfile,
            Role::Pidfile(file(dir.path(), "app.pid")),
        );
        let names: Vec<_> = files
            .owned(None)
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert_eq!(names, ["lockfile", "pidfile"]);
    }

    /// The user configured that file as the pidfile, so a collision names it
    /// so, though step 7 opened it as the lock.
    #[test]
    fn a_lockfile_that_is_the_pidfile_is_named_the_pidfile() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let pidfile = dir.path().join("app.pid");
        push(
            &mut files,
            &pidfile,
            Role::Lockfile(lock(dir.path(), "app.pid")),
        );
        let id = files.owned(None)[0].1;

        assert_eq!(files.owned(Some(id))[0].0, "pidfile");
    }

    #[test]
    fn stream_files_are_not_owned() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let out = dir.path().join("out.log");
        push(&mut files, &out, Role::Stream(file(dir.path(), "out.log")));

        assert!(files.owned(None).is_empty());
    }

    #[test]
    fn an_empty_record_spares_only_the_held_and_the_pipe() {
        let files = OpenedFiles::default();
        assert_eq!(spared_fds(&files, [7, 8], Some(9)), [7, 8, 9]);
    }

    #[test]
    fn every_file_in_the_record_is_spared() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let lockfile = dir.path().join("app.lock");
        let pidfile = dir.path().join("app.pid");
        let out = dir.path().join("out.log");
        push(
            &mut files,
            &lockfile,
            Role::Lockfile(lock(dir.path(), "app.lock")),
        );
        push(
            &mut files,
            &pidfile,
            Role::Pidfile(file(dir.path(), "app.pid")),
        );
        push(&mut files, &out, Role::Stream(file(dir.path(), "out.log")));
        let expected: Vec<i32> = files.0.iter().map(|o| o.role.as_raw_fd()).collect();

        assert_eq!(spared_fds(&files, [], None), expected);
    }

    #[test]
    fn the_lock_is_the_lockfile_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let pidfile = dir.path().join("app.pid");
        let lockfile = dir.path().join("app.lock");
        push(
            &mut files,
            &pidfile,
            Role::Pidfile(file(dir.path(), "app.pid")),
        );
        push(
            &mut files,
            &lockfile,
            Role::Lockfile(lock(dir.path(), "app.lock")),
        );
        let raw = files.0[1].role.as_raw_fd();

        assert_eq!(files.take_lock().map(|l| l.as_raw_fd()), Some(raw));
    }

    #[test]
    fn a_record_without_a_lockfile_has_no_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let out = dir.path().join("out.log");
        push(&mut files, &out, Role::Stream(file(dir.path(), "out.log")));

        assert!(files.lock().is_none());
    }
}
