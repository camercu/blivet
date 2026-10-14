//! The files the sequence opened at steps 7, 8 and 12: one record, which step
//! 13's fd sweep (R152) and step 16's chown (R163) both read.
//!
//! Each list was assembled by hand before, and a file added to one was missed
//! in the other. Both now derive from the record, and only the steps that
//! open files write it. stdio slots are never in it, so step 16 chowns the
//! files as opened, not whatever `init` left on fds 1 and 2.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::Path;

use nix::fcntl::Flock;

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

/// One opened file: the path it was opened by, and its role.
struct Opened<'a> {
    path: &'a Path,
    role: Role,
}

/// The files the sequence opened, in the order it opened them.
#[derive(Default)]
pub(crate) struct OpenedFiles<'a>(Vec<Opened<'a>>);

impl<'a> OpenedFiles<'a> {
    /// Records a file a step opened.
    pub(crate) fn push(&mut self, path: &'a Path, role: Role) {
        self.0.push(Opened { path, role });
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
/// record, each descriptor an undo token holds, and the notification pipe's
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

    fn file(dir: &Path, name: &str) -> OwnedFd {
        std::fs::File::create(dir.join(name)).unwrap().into()
    }

    fn lock(dir: &Path, name: &str) -> nix::fcntl::Flock<OwnedFd> {
        crate::steps::open_and_lock(&dir.join(name)).unwrap()
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
        files.push(&lockfile, Role::Lockfile(lock(dir.path(), "app.lock")));
        files.push(&pidfile, Role::Pidfile(file(dir.path(), "app.pid")));
        files.push(&out, Role::Stream(file(dir.path(), "out.log")));
        let expected: Vec<i32> = files.0.iter().map(|o| o.role.as_raw_fd()).collect();

        assert_eq!(spared_fds(&files, [], None), expected);
    }

    #[test]
    fn every_file_in_the_record_is_a_chown_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let lockfile = dir.path().join("app.lock");
        let out = dir.path().join("out.log");
        files.push(&lockfile, Role::Lockfile(lock(dir.path(), "app.lock")));
        files.push(&out, Role::Stream(file(dir.path(), "out.log")));

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
        files.push(&out, Role::Stream(file(dir.path(), "out.log")));

        assert!(chown_targets(&files, false).is_empty());
    }

    #[test]
    fn the_lock_is_the_lockfile_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let pidfile = dir.path().join("app.pid");
        let lockfile = dir.path().join("app.lock");
        files.push(&pidfile, Role::Pidfile(file(dir.path(), "app.pid")));
        files.push(&lockfile, Role::Lockfile(lock(dir.path(), "app.lock")));
        let raw = files.0[1].role.as_raw_fd();

        assert_eq!(files.take_lock().map(|l| l.as_raw_fd()), Some(raw));
    }

    #[test]
    fn a_record_without_a_lockfile_has_no_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = OpenedFiles::default();
        let out = dir.path().join("out.log");
        files.push(&out, Role::Stream(file(dir.path(), "out.log")));

        assert!(files.lock().is_none());
    }
}
