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

use crate::error::{ChangedState, RestoreFailure};
use crate::file_identity::{FileId, stat_if_named};

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

/// A file a step left on disk, to remove on failure in either mode (R164).
///
/// Removed only while the path still names it: a file another process put
/// there since is not this one's to remove.
pub(crate) struct Removal {
    path: PathBuf,
    id: FileId,
    state: ChangedState,
}

impl Removal {
    pub(crate) fn new(path: &Path, id: FileId, state: ChangedState) -> Self {
        Self {
            path: path.to_path_buf(),
            id,
            state,
        }
    }

    pub(crate) fn restore(self) -> Result<(), RestoreFailure> {
        let failed = |e: std::io::Error| RestoreFailure::at(self.state, &self.path, e);
        match stat_if_named(&self.path, self.id) {
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
}

/// The files the sequence opened, in the order it opened them.
#[derive(Default)]
pub(crate) struct OpenedFiles<'a>(Vec<Opened<'a>>);

impl<'a> OpenedFiles<'a> {
    /// Records a file a step opened, with the identity `fstat` gave it.
    pub(crate) fn push(&mut self, path: &'a Path, role: Role, id: FileId) {
        self.0.push(Opened { path, role, id });
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

/// Step 16: the files to hand to the target identity, by descriptor (R163);
/// none without `chown_paths`.
pub(crate) fn chown_targets<'f>(
    files: &'f OpenedFiles,
    chown_paths: bool,
) -> Vec<(&'f Path, BorrowedFd<'f>)> {
    if !chown_paths {
        return Vec::new();
    }
    files.0.iter().map(|o| (o.path, o.role.as_fd())).collect()
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
    fn every_file_in_the_record_is_a_chown_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let lockfile = dir.path().join("app.lock");
        let out = dir.path().join("out.log");
        push(
            &mut files,
            &lockfile,
            Role::Lockfile(lock(dir.path(), "app.lock")),
        );
        push(&mut files, &out, Role::Stream(file(dir.path(), "out.log")));

        let targets: Vec<_> = chown_targets(&files, true)
            .into_iter()
            .map(|(path, fd)| (path.to_owned(), fd.as_raw_fd()))
            .collect();

        let expected: Vec<_> = files
            .0
            .iter()
            .map(|o| (o.path.to_owned(), o.role.as_raw_fd()))
            .collect();
        assert_eq!(targets, expected);
    }

    #[test]
    fn without_chown_paths_nothing_is_a_chown_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let out = dir.path().join("out.log");
        push(&mut files, &out, Role::Stream(file(dir.path(), "out.log")));

        assert!(chown_targets(&files, false).is_empty());
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
