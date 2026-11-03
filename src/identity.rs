//! The identity the daemon runs as: resolving the configured user and group
//! specs (a name **or** a numeric id), handing the daemon's files to that
//! identity, and switching to it.
//!
//! A spec is the raw `String` a caller configures via
//! [`DaemonConfig::user`](crate::DaemonConfig::user) /
//! [`group`](crate::DaemonConfig::group). The daemonize sequence resolves it
//! before the caller's init runs, so a missing user fails before init's side
//! effects, then chowns and switches after init (ADR 0004).

use std::ffi::CString;
use std::path::{Path, PathBuf};

use nix::unistd::{Gid, Group, Uid, User};

use crate::error::{ChangedState, DaemonizeError, ErrorKind, RestoreFailure};

/// A user resolved from a name or numeric UID spec.
pub(crate) struct ResolvedUser {
    pub(crate) name: String,
    pub(crate) uid: Uid,
    pub(crate) gid: Gid,
    pub(crate) dir: PathBuf,
}

impl ResolvedUser {
    /// The username as a `CString` for `initgroups`.
    ///
    /// # Errors
    ///
    /// Returns `ErrorKind::UserNotFound` if the name contains a NUL byte.
    pub(crate) fn cname(&self) -> Result<CString, ErrorKind> {
        CString::new(self.name.as_str())
            .map_err(|e| ErrorKind::UserNotFound(format!("invalid username: {e}")))
    }
}

/// The target identity to chown to and switch to, resolved once from
/// the configured user and group specs.
pub(crate) struct ResolvedIdentity {
    user: Option<ResolvedUser>,
    explicit_group: Option<Gid>,
}

impl ResolvedIdentity {
    /// Resolve the user and group specs. Each may be a name or a numeric id.
    ///
    /// # Errors
    ///
    /// Returns `ErrorKind::UserNotFound` / `GroupNotFound` if a configured
    /// spec cannot be resolved.
    pub(crate) fn resolve(user: Option<&str>, group: Option<&str>) -> Result<Self, ErrorKind> {
        let user = match user {
            Some(spec) => Some(resolve_user(spec)?),
            None => None,
        };
        let explicit_group = match group {
            Some(spec) => Some(resolve_group_gid(spec)?),
            None => None,
        };
        Ok(Self {
            user,
            explicit_group,
        })
    }

    /// The resolved user, if a user spec was configured.
    pub(crate) fn user(&self) -> Option<&ResolvedUser> {
        self.user.as_ref()
    }

    /// The gid to `setgid`/`chown` to: the explicit group if configured,
    /// otherwise the user's primary group. `None` when neither is set.
    pub(crate) fn effective_gid(&self) -> Option<Gid> {
        self.explicit_group
            .or_else(|| self.user.as_ref().map(|u| u.gid))
    }

    /// The `(owner, group)` pair for `chown`. `None` in either position means
    /// "leave unchanged" — [`nix::unistd::chown`] interprets `None` exactly so,
    /// which is why this replaces the former `u32::MAX` (`-1`) sentinel.
    pub(crate) fn chown_ids(&self) -> (Option<Uid>, Option<Gid>) {
        (self.user.as_ref().map(|u| u.uid), self.effective_gid())
    }

    /// Chown each open regular file to this identity, while the process is
    /// still privileged (R163). `path` names the file in the error.
    ///
    /// By descriptor, not path: the chown reaches the file the sequence opened,
    /// even if `init` has since changed what the path names, by a `chroot` or
    /// a rename. A symlink planted before the open still redirects the open
    /// itself, so these paths belong in directories untrusted users cannot
    /// write.
    ///
    /// Records a token per file chowned in `ledger`, in both modes: the
    /// owner is on disk, so it outlives a daemon that exits (R164). Each goes
    /// in as its file is chowned, so a failure part way leaves the ledger to
    /// give back the owners of the files chowned before it.
    ///
    /// # Errors
    ///
    /// Returns `ErrorKind::ChownError` naming the first file that fails.
    pub(crate) fn chown_files<'p>(
        &self,
        files: impl IntoIterator<Item = (&'p Path, std::os::fd::BorrowedFd<'p>)>,
        ledger: &mut crate::steps::Ledger,
    ) -> Result<(), ErrorKind> {
        use crate::steps::{Saved, Undo};
        let (owner, group) = self.chown_ids();
        let failed = |path: &Path, e| ErrorKind::ChownError(format!("{}: {e}", path.display()));
        for (path, fd) in files {
            let st = nix::sys::stat::fstat(fd).map_err(|e| failed(path, e))?;
            // Only a regular file is the daemon's own. A device, FIFO, or
            // socket (`-o /dev/null`, a tty) belongs to the system; the
            // daemon writes through the descriptor it holds either way.
            if st.st_mode & libc::S_IFMT != libc::S_IFREG {
                continue;
            }
            // Its own descriptor, above stdio and close-on-exec, as the
            // sequence's copy closes when the sequence ends.
            let held = fd.try_clone_to_owned();
            nix::unistd::fchown(fd, owner, group).map_err(|e| failed(path, e))?;
            ledger.record(Undo::new(Saved::Owner(Owner {
                path: path.to_path_buf(),
                fd: held,
                id: crate::file_identity::FileId::of(&st),
                was: (Uid::from_raw(st.st_uid), Gid::from_raw(st.st_gid)),
            })))?;
        }
        Ok(())
    }

    /// Set the supplementary groups to the user's, from the group database
    /// (`initgroups`). A no-op without a user.
    ///
    /// The sequence calls this before the caller's `init`, so an `init` that
    /// changes root cannot change which group database is read (R162). `init`
    /// then runs with the user's groups in place of the caller's: a full
    /// root's checks pass either way, but a root bounded by capabilities (no
    /// `CAP_DAC_OVERRIDE`) has the group access of the user, not its own.
    ///
    /// # Errors
    ///
    /// Returns `ErrorKind::PermissionDenied` if `initgroups` fails, and
    /// `UserNotFound` for a name with a NUL byte.
    pub(crate) fn join_groups(
        &self,
        rollback: crate::steps::Rollback,
    ) -> Result<crate::steps::Undo, ErrorKind> {
        use crate::steps::{Saved, Undo};
        #[cfg(test)]
        crate::test_support::assert_isolated("joining the user's groups");
        let Some(info) = self.user() else {
            return Ok(Undo::nothing());
        };
        let undo = Undo::saving(rollback, || {
            Saved::Groups(Groups(crate::sys::raw_getgroups()))
        });
        crate::sys::raw_initgroups(&info.cname()?, info.gid.as_raw())
            .map_err(|e| ErrorKind::PermissionDenied(format!("initgroups: {e}")))?;
        Ok(undo)
    }

    /// Switch the process to this identity.
    ///
    /// - User: `setgid` to the explicit group or else the user's primary
    ///   group, `setuid`, then `USER`, `HOME`, `LOGNAME` (overriding any
    ///   configured `.env()` values). The supplementary groups were set
    ///   earlier, by [`join_groups`](Self::join_groups).
    /// - Group only: `setgid`.
    ///
    /// # Safety
    ///
    /// With a user, this calls `setenv`, which is not thread-safe: no other
    /// thread may be running.
    ///
    /// # Errors
    ///
    /// Returns `ErrorKind::PermissionDenied` if `setgid` or `setuid`
    /// fails.
    #[allow(unsafe_code)]
    pub(crate) unsafe fn switch_to(&self) -> Result<(), DaemonizeError> {
        #[cfg(test)]
        crate::test_support::assert_isolated("the identity switch");
        let before = (nix::unistd::getgid(), nix::unistd::getegid());
        if let Some(gid) = self.effective_gid() {
            nix::unistd::setgid(gid)
                .map_err(|e| ErrorKind::PermissionDenied(format!("setgid: {e}")))?;
        }
        if let Some(info) = self.user() {
            #[cfg(test)]
            let switched = if crate::steps::failpoints::SETUID_FAILS.injected() {
                Err(nix::errno::Errno::EAGAIN)
            } else {
                nix::unistd::setuid(info.uid)
            };
            #[cfg(not(test))]
            let switched = nix::unistd::setuid(info.uid);
            if let Err(e) = switched {
                // Still privileged, since setuid failed: put the group back
                // rather than leave a half-switched identity (R164). The saved
                // gid follows the real one; R164 lists that.
                let gid = nix::unistd::setgid(before.0);
                let egid = nix::unistd::setegid(before.1);
                let failure = gid
                    .and(egid)
                    .err()
                    .map(|e| RestoreFailure::new(ChangedState::PrimaryGroup, e));
                let failed = ErrorKind::PermissionDenied(format!("setuid: {e}"));
                return Err(DaemonizeError::from(failed).with_rollback_failures(failure));
            }
            for (key, value) in [
                ("USER", info.name.as_ref()),
                ("HOME", info.dir.as_os_str()),
                ("LOGNAME", info.name.as_ref()),
            ] {
                // SAFETY: this function's contract requires a single thread,
                // so the `setenv` cannot race another thread's `environ` read.
                #[allow(clippy::disallowed_methods)]
                // setenv thread safety: single thread, per the contract
                unsafe {
                    std::env::set_var(key, value)
                };
            }
        }
        Ok(())
    }
}

/// Resolve a user spec (name or numeric UID string).
fn resolve_user(spec: &str) -> Result<ResolvedUser, ErrorKind> {
    let user = if let Ok(uid_num) = spec.parse::<u32>() {
        let uid = Uid::from_raw(uid_num);
        User::from_uid(uid)
            .map_err(|e| ErrorKind::UserNotFound(format!("getpwuid({uid_num}): {e}")))?
            .ok_or_else(|| ErrorKind::UserNotFound(format!("uid {uid_num}")))?
    } else {
        User::from_name(spec)
            .map_err(|e| ErrorKind::UserNotFound(format!("getpwnam({spec}): {e}")))?
            .ok_or_else(|| ErrorKind::UserNotFound(spec.to_string()))?
    };
    Ok(ResolvedUser {
        name: user.name,
        uid: user.uid,
        gid: user.gid,
        dir: user.dir,
    })
}

/// Resolve a group spec (name or numeric GID string) to a GID.
fn resolve_group_gid(spec: &str) -> Result<Gid, ErrorKind> {
    if let Ok(gid_num) = spec.parse::<u32>() {
        Ok(Gid::from_raw(gid_num))
    } else {
        let group = Group::from_name(spec)
            .map_err(|e| ErrorKind::GroupNotFound(format!("getgrnam({spec}): {e}")))?
            .ok_or_else(|| ErrorKind::GroupNotFound(spec.to_string()))?;
        Ok(group.gid)
    }
}

/// A file step 16 chowned, and the owner and group it had before.
///
/// The descriptor to give it back by is best-effort, as the working
/// directory's is: with no descriptor free it is missing, and the chown goes
/// on (R164). The restore then goes by path.
pub(crate) struct Owner {
    path: std::path::PathBuf,
    fd: std::io::Result<std::os::fd::OwnedFd>,
    id: crate::file_identity::FileId,
    was: (Uid, Gid),
}

impl Owner {
    /// Gives the file its owner back by descriptor. Without one, by path,
    /// while the path still names the file: one a later token removes, or
    /// one gone already, is nothing to report.
    pub(crate) fn restore(self) -> Result<(), RestoreFailure> {
        let Self { path, fd, id, was } = self;
        let failed = |e: nix::errno::Errno| {
            RestoreFailure::at(ChangedState::FileOwner, &path, std::io::Error::from(e))
        };
        let (owner, group) = (Some(was.0), Some(was.1));
        match fd {
            Ok(fd) => nix::unistd::fchown(&fd, owner, group).map_err(failed),
            Err(_) => match crate::file_identity::stat_if_named(&path, id).map_err(failed)? {
                Some(_) => nix::unistd::fchownat(
                    nix::fcntl::AT_FDCWD,
                    &path,
                    owner,
                    group,
                    nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
                )
                .map_err(failed),
                None => Ok(()),
            },
        }
    }
}

/// The supplementary groups step 14 replaced. Best-effort, as the working
/// directory is: a `getgroups` failure must not fail the start (R164). The
/// restore then reports why it could not put them back.
pub(crate) struct Groups(Result<Vec<libc::gid_t>, nix::errno::Errno>);

impl Groups {
    pub(crate) fn restore(self) -> Result<(), RestoreFailure> {
        self.0
            .and_then(|groups| crate::sys::raw_setgroups(&groups))
            .map_err(|e| RestoreFailure::new(ChangedState::SupplementaryGroups, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Covers: R63
    #[test]
    fn resolve_user_numeric() {
        // UID 0 should resolve to root on all Unix systems
        let user = resolve_user("0").unwrap();
        assert_eq!(user.uid.as_raw(), 0);
        assert_eq!(user.name, "root");
    }

    // Covers: R63
    #[test]
    fn resolve_user_numeric_reports_what_the_passwd_database_says() {
        // Switching to a user needs the passwd entry's primary GID and home
        // directory, so a numeric user is looked up rather than taken at face
        // value, and an id the database does not know cannot be honoured.
        //
        // Which ids those are is not universal: bionic synthesises an entry for
        // ids in Android's AID ranges, so the same number that is unassigned on
        // Linux resolves there. So ask the database first and hold the crate to
        // its answer — that is the requirement, and it is testable on every
        // platform. A test that simply expected rejection passed everywhere the
        // library is developed and failed on the bionic tier.
        //
        // The database may also answer with an error rather than an entry or
        // none: the Android emulator does for this id. An id it cannot look up
        // cannot be honoured either, so that counts as unknown.
        const UID: u32 = 4_242_424;
        let known = matches!(User::from_uid(Uid::from_raw(UID)), Ok(Some(_)));

        match (known, resolve_user(&UID.to_string())) {
            (true, Ok(user)) => assert_eq!(
                user.uid.as_raw(),
                UID,
                "resolved the wrong uid for an id the database knows"
            ),
            (false, Err(ErrorKind::UserNotFound(_))) => {}
            (false, Ok(user)) => panic!(
                "the database has no entry for {UID}, but it resolved to {:?}",
                user.name
            ),
            (_, Err(e)) => {
                panic!("the database has an entry for {UID}, but resolving it failed: {e:?}")
            }
        }
    }

    // Covers: R142
    #[test]
    fn resolve_group_numeric_unassigned_is_accepted() {
        // Deliberately not symmetric with the user side: setgid() needs nothing
        // the number does not carry, and a GID with no group entry is ordinary
        // on Unix, so refusing one would reject a legitimate configuration.
        let gid = resolve_group_gid("4242424").expect("an unassigned gid is usable");
        assert_eq!(gid.as_raw(), 4242424);
    }

    // Covers: R59
    #[test]
    fn resolve_user_name() {
        let user = resolve_user("root").unwrap();
        assert_eq!(user.uid.as_raw(), 0);
    }

    /// Ignored by default: a name that does not exist is the lookup NSS can
    /// take longest to refuse, and nothing here bounds it. The glibc container
    /// tier runs it with `--include-ignored`, where the sources are local.
    #[test]
    #[ignore = "getpwnam for a missing user can block on a networked NSS source"]
    fn resolve_user_nonexistent_name() {
        let result = resolve_user("nonexistent_daemonize_test_user_xyz");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_group_gid_numeric() {
        let gid = resolve_group_gid("0").unwrap();
        assert_eq!(gid.as_raw(), 0);
    }

    #[test]
    fn resolve_group_gid_by_name() {
        // "root" on Linux, "wheel" on macOS/BSD — try root first to avoid
        // NSS lookup hangs for nonexistent groups in CI.
        let result = resolve_group_gid("root").or_else(|_| resolve_group_gid("wheel"));
        assert!(result.is_ok());
    }

    /// Ignored for the same reason as [`resolve_user_nonexistent_name`].
    // Covers: R51
    #[test]
    #[ignore = "getgrnam for a missing group can block on a networked NSS source"]
    fn resolve_group_gid_nonexistent_name() {
        let result = resolve_group_gid("nonexistent_daemonize_test_group_xyz");
        assert!(matches!(result, Err(ErrorKind::GroupNotFound(_))));
    }

    #[test]
    fn chown_ids_user_only() {
        // User only: owner is the user's UID, group is the user's primary GID.
        let id = ResolvedIdentity::resolve(Some("root"), None).unwrap();
        let (uid, gid) = id.chown_ids();
        assert_eq!(uid.unwrap().as_raw(), 0);
        assert_eq!(gid.unwrap().as_raw(), 0); // root's primary group
    }

    // Covers: R62
    #[test]
    fn chown_ids_neither_leaves_unchanged() {
        // Neither configured: both positions None (leave unchanged).
        let id = ResolvedIdentity::resolve(None, None).unwrap();
        assert_eq!(id.chown_ids(), (None, None));
    }

    // Covers: R61
    #[test]
    fn chown_ids_group_only_numeric() {
        // Group only: owner unchanged, group set to the numeric GID.
        let id = ResolvedIdentity::resolve(None, Some("0")).unwrap();
        let (uid, gid) = id.chown_ids();
        assert!(uid.is_none()); // no user change
        assert_eq!(gid.unwrap().as_raw(), 0);
    }

    #[test]
    fn user_accessor_returns_resolved_user() {
        let id = ResolvedIdentity::resolve(Some("root"), None).unwrap();
        assert_eq!(id.user().expect("user was configured").name, "root");

        let id = ResolvedIdentity::resolve(None, None).unwrap();
        assert!(id.user().is_none());
    }

    #[test]
    fn cname_roundtrips_the_username() {
        let id = ResolvedIdentity::resolve(Some("root"), None).unwrap();
        let cname = id.user().unwrap().cname().unwrap();
        assert_eq!(cname.as_bytes(), b"root");
    }

    #[test]
    fn effective_gid_prefers_explicit_group() {
        // Explicit group overrides the user's primary group.
        let id = ResolvedIdentity::resolve(Some("root"), Some("0")).unwrap();
        assert_eq!(id.effective_gid().unwrap().as_raw(), 0);
    }
}
