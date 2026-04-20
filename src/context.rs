//! Post-daemonization context: parent notification, lockfile management,
//! privilege dropping, and path ownership.

use std::fmt;
use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;

use nix::fcntl::Flock;

use crate::error::DaemonizeError;

/// Context returned by a successful daemonization.
///
/// Returned by [`daemonize()`](crate::daemonize) while the process still
/// has its original privileges. This split-phase design lets callers
/// perform privileged work (bind low ports, open devices) *after*
/// daemonizing but *before* dropping to an unprivileged user.  See the
/// [crate-level docs](crate#split-phase-design) for the full rationale.
///
/// Holds the lockfile (if configured), the notification pipe write end,
/// and cloned configuration fields needed for post-daemonization
/// operations like privilege dropping and path ownership changes.
///
/// Dropping this without calling [`notify_parent`](DaemonContext::notify_parent)
/// writes a failure message to the notification pipe, causing the parent to
/// exit non-zero.
///
/// The lock is released when this value is dropped.
///
/// # Post-daemonization workflow
///
/// When privilege dropping is needed, the recommended call order is:
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let config = blivet::DaemonConfig::new();
/// let mut ctx = unsafe { blivet::daemonize(&config)? };
/// // ... privileged work (e.g., bind port 80) ...
/// ctx.chown_paths()?;       // transfer file ownership while still root
/// ctx.drop_privileges()?;   // setgid + setuid
/// ctx.notify_parent()?;     // signal readiness to parent
/// # Ok(())
/// # }
/// ```
#[non_exhaustive]
pub struct DaemonContext {
    lockfile: Option<Flock<OwnedFd>>,
    notify_pipe: Option<OwnedFd>,
    pidfile: Option<PathBuf>,
    lockfile_path: Option<PathBuf>,
    stdout: Option<PathBuf>,
    stderr: Option<PathBuf>,
    user: Option<String>,
    group: Option<String>,
}

impl fmt::Debug for DaemonContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        /// Helper that prints the inner value or `"none"`, avoiding `Some(…)` noise.
        struct OptFmt<'a, T: fmt::Debug>(&'a Option<T>);
        impl<T: fmt::Debug> fmt::Debug for OptFmt<'_, T> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self.0 {
                    Some(v) => v.fmt(f),
                    None => f.write_str("none"),
                }
            }
        }

        f.debug_struct("DaemonContext")
            .field("lockfile", &OptFmt(&self.lockfile.as_ref().map(|_| "held")))
            .field(
                "notify_pipe",
                &OptFmt(&self.notify_pipe.as_ref().map(|_| "open")),
            )
            .field("pidfile", &OptFmt(&self.pidfile))
            .field("lockfile_path", &OptFmt(&self.lockfile_path))
            .field("stdout", &OptFmt(&self.stdout))
            .field("stderr", &OptFmt(&self.stderr))
            .field("user", &OptFmt(&self.user))
            .field("group", &OptFmt(&self.group))
            .finish()
    }
}

impl DaemonContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        lockfile: Option<Flock<OwnedFd>>,
        notify_pipe: Option<OwnedFd>,
        pidfile: Option<PathBuf>,
        lockfile_path: Option<PathBuf>,
        stdout: Option<PathBuf>,
        stderr: Option<PathBuf>,
        user: Option<String>,
        group: Option<String>,
    ) -> Self {
        Self {
            lockfile,
            notify_pipe,
            pidfile,
            lockfile_path,
            stdout,
            stderr,
            user,
            group,
        }
    }

    /// Returns a borrowed reference to the lockfile fd, or `None` if no
    /// lockfile was configured.
    ///
    /// The returned fd has `O_CLOEXEC` set. If you intend to `exec` and want
    /// the lock to survive, clear `CLOEXEC` before calling `exec`.
    pub fn lockfile_fd(&self) -> Option<BorrowedFd<'_>> {
        self.lockfile.as_ref().map(|flock| flock.as_fd())
    }

    /// Changes ownership of all configured path-based resources to the target
    /// user/group.
    ///
    /// Chowns pidfile, lockfile, stdout, and stderr files when they are
    /// configured. Must be called while still privileged (before
    /// [`drop_privileges`](DaemonContext::drop_privileges)). No-op if neither
    /// user nor group is configured.
    ///
    /// # Errors
    ///
    /// Returns `DaemonizeError::ChownError` if `chown()` fails on any path.
    /// Returns `DaemonizeError::UserNotFound` or `DaemonizeError::GroupNotFound`
    /// if the configured user/group cannot be resolved.
    pub fn chown_paths(&mut self) -> Result<(), DaemonizeError> {
        if self.user.is_none() && self.group.is_none() {
            return Ok(());
        }

        let (uid, gid) = resolve_uid_gid(self.user.as_deref(), self.group.as_deref())?;
        let owner = Some(nix::unistd::Uid::from_raw(uid));
        let group = Some(nix::unistd::Gid::from_raw(gid));

        let paths: Vec<&PathBuf> = [
            &self.pidfile,
            &self.lockfile_path,
            &self.stdout,
            &self.stderr,
        ]
        .iter()
        .filter_map(|p| p.as_ref())
        .collect();

        for path in paths {
            if path.exists() {
                nix::unistd::chown(path, owner, group)
                    .map_err(|e| DaemonizeError::ChownError(format!("{}: {e}", path.display())))?;
            }
        }

        Ok(())
    }

    /// Drops privileges by switching user and/or group.
    ///
    /// Resolution: if the user/group string parses as a `u32`, it is treated
    /// as a numeric UID/GID. Otherwise, it is resolved via `getpwnam()` /
    /// `getgrnam()`.
    ///
    /// Four combinations:
    ///
    /// - Neither user nor group configured: no-op.
    /// - User only: `initgroups` + `setgid(primary_gid)` + `setuid(uid)`.
    /// - User and group: `initgroups` + `setgid(group_gid)` + `setuid(uid)`.
    /// - Group only: `setgid(group_gid)`.
    ///
    /// After switching, sets `USER`, `HOME`, `LOGNAME` environment variables.
    ///
    /// # Safety considerations
    ///
    /// This method calls `setenv` internally (to set `USER`, `HOME`,
    /// `LOGNAME`), which is not thread-safe. Do not spawn threads between
    /// [`daemonize()`](crate::daemonize) and this call.
    ///
    /// # Errors
    ///
    /// Returns `DaemonizeError::UserNotFound` if the user cannot be resolved.
    /// Returns `DaemonizeError::GroupNotFound` if the group cannot be resolved.
    /// Returns `DaemonizeError::PermissionDenied` if `initgroups`, `setgid`,
    /// or `setuid` fails.
    pub fn drop_privileges(&mut self) -> Result<(), DaemonizeError> {
        use std::ffi::CString;

        if self.user.is_none() && self.group.is_none() {
            return Ok(());
        }

        let user_info = match self.user.as_deref() {
            Some(spec) => Some(resolve_user(spec)?),
            None => None,
        };

        let group_gid = match self.group.as_deref() {
            Some(spec) => Some(resolve_group_gid(spec)?),
            None => None,
        };

        if let Some(ref info) = user_info {
            let cname = CString::new(info.name.as_str())
                .map_err(|e| DaemonizeError::UserNotFound(format!("invalid username: {e}")))?;

            crate::unsafe_ops::raw_initgroups(&cname, info.gid.as_raw())
                .map_err(|e| DaemonizeError::PermissionDenied(format!("initgroups: {e}")))?;
        }

        // setgid: use explicit group if set, otherwise user's primary group
        let effective_gid = group_gid.or(user_info.as_ref().map(|u| u.gid));
        if let Some(gid) = effective_gid {
            nix::unistd::setgid(gid)
                .map_err(|e| DaemonizeError::PermissionDenied(format!("setgid: {e}")))?;
        }

        // setuid
        if let Some(ref info) = user_info {
            nix::unistd::setuid(info.uid)
                .map_err(|e| DaemonizeError::PermissionDenied(format!("setuid: {e}")))?;

            // Set USER, HOME, LOGNAME — overwrite any .env() values
            crate::unsafe_ops::raw_set_env_var("USER", &info.name);
            crate::unsafe_ops::raw_set_env_var("HOME", &info.dir);
            crate::unsafe_ops::raw_set_env_var("LOGNAME", &info.name);
        }

        Ok(())
    }

    /// Signals the parent that the daemon is ready.
    ///
    /// Writes a success byte (`0x00`) to the notification pipe and closes it.
    /// The parent reads this and exits 0.
    ///
    /// After this call, subsequent calls are no-ops (the pipe is consumed).
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if writing to the pipe fails.
    #[must_use = "the parent process blocks until notified; ignoring this Result may leave it waiting"]
    pub fn notify_parent(&mut self) -> Result<(), io::Error> {
        if let Some(fd) = self.notify_pipe.take() {
            let mut file = io::BufWriter::new(std::fs::File::from(fd));
            file.write_all(&[0x00])?;
            file.flush()?;
        }
        Ok(())
    }

    /// Reports an error to the parent process and exits.
    ///
    /// Writes the error's exit code byte followed by the `Display` message to
    /// the notification pipe, then calls `_exit()`. The parent reads this,
    /// prints the message to stderr, and exits with the code.
    ///
    /// Uses `libc::_exit` rather than `std::process::exit` to avoid running
    /// atexit handlers or flushing stdio buffers inherited from the pre-fork
    /// parent, which could cause double-flush corruption or deadlocks.
    pub fn report_error(&mut self, err: &DaemonizeError) -> ! {
        if let Some(fd) = self.notify_pipe.take() {
            let mut file = io::BufWriter::new(std::fs::File::from(fd));
            let msg = err.to_string();
            let code = err.exit_code();
            let mut buf = Vec::with_capacity(1 + msg.len());
            buf.push(code);
            buf.extend_from_slice(msg.as_bytes());
            let _ = file.write_all(&buf);
            let _ = file.flush();
        }
        crate::unsafe_ops::raw_exit(err.exit_code() as i32)
    }
}

impl Drop for DaemonContext {
    fn drop(&mut self) {
        if let Some(fd) = self.notify_pipe.take() {
            let mut file = io::BufWriter::new(std::fs::File::from(fd));
            let msg = b"daemon exited without signaling readiness";
            let mut buf = Vec::with_capacity(1 + msg.len());
            buf.push(1u8);
            buf.extend_from_slice(msg);
            let _ = file.write_all(&buf);
            let _ = file.flush();
        }
    }
}

/// Resolved user info from getpwnam or numeric UID.
struct ResolvedUser {
    name: String,
    uid: nix::unistd::Uid,
    gid: nix::unistd::Gid,
    dir: std::path::PathBuf,
}

/// Resolve a user spec (name or numeric UID string).
fn resolve_user(spec: &str) -> Result<ResolvedUser, DaemonizeError> {
    use nix::unistd::User;

    if let Ok(uid_num) = spec.parse::<u32>() {
        let uid = nix::unistd::Uid::from_raw(uid_num);
        let user = User::from_uid(uid)
            .map_err(|e| DaemonizeError::UserNotFound(format!("getpwuid({uid_num}): {e}")))?
            .ok_or_else(|| DaemonizeError::UserNotFound(format!("uid {uid_num}")))?;
        Ok(ResolvedUser {
            name: user.name,
            uid: user.uid,
            gid: user.gid,
            dir: user.dir,
        })
    } else {
        let user = User::from_name(spec)
            .map_err(|e| DaemonizeError::UserNotFound(format!("getpwnam({spec}): {e}")))?
            .ok_or_else(|| DaemonizeError::UserNotFound(spec.to_string()))?;
        Ok(ResolvedUser {
            name: user.name,
            uid: user.uid,
            gid: user.gid,
            dir: user.dir,
        })
    }
}

/// Resolve a group spec (name or numeric GID string) to a GID.
fn resolve_group_gid(spec: &str) -> Result<nix::unistd::Gid, DaemonizeError> {
    use nix::unistd::Group;

    if let Ok(gid_num) = spec.parse::<u32>() {
        Ok(nix::unistd::Gid::from_raw(gid_num))
    } else {
        let group = Group::from_name(spec)
            .map_err(|e| DaemonizeError::GroupNotFound(format!("getgrnam({spec}): {e}")))?
            .ok_or_else(|| DaemonizeError::GroupNotFound(spec.to_string()))?;
        Ok(group.gid)
    }
}

/// Resolve user/group specs to (uid_t, gid_t) for chown.
/// Returns `(u32::MAX, u32::MAX)` for fields that should be unchanged.
fn resolve_uid_gid(
    user: Option<&str>,
    group: Option<&str>,
) -> Result<(libc::uid_t, libc::gid_t), DaemonizeError> {
    let resolved_user = match user {
        Some(spec) => Some(resolve_user(spec)?),
        None => None,
    };
    let uid = resolved_user.as_ref().map_or(u32::MAX, |u| u.uid.as_raw());
    let gid = match group {
        Some(spec) => resolve_group_gid(spec)?.as_raw(),
        // If user is set but group isn't, use user's primary group
        None => resolved_user.as_ref().map_or(u32::MAX, |u| u.gid.as_raw()),
    };
    Ok((uid, gid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn make_pipe() -> (OwnedFd, OwnedFd) {
        nix::unistd::pipe().unwrap()
    }

    fn read_pipe(rd: OwnedFd) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut file = std::fs::File::from(rd);
        file.read_to_end(&mut buf).unwrap();
        buf
    }

    #[test]
    fn notify_parent_writes_success_byte() {
        let (rd, wr) = make_pipe();
        let mut ctx = DaemonContext::new(None, Some(wr), None, None, None, None, None, None);
        ctx.notify_parent().unwrap();
        assert_eq!(read_pipe(rd), vec![0x00]);
    }

    #[test]
    fn notify_parent_idempotent() {
        let (_rd, wr) = make_pipe();
        let mut ctx = DaemonContext::new(None, Some(wr), None, None, None, None, None, None);
        ctx.notify_parent().unwrap();
        ctx.notify_parent().unwrap();
    }

    #[test]
    fn drop_writes_failure_when_not_notified() {
        let (rd, wr) = make_pipe();
        {
            let _ctx = DaemonContext::new(None, Some(wr), None, None, None, None, None, None);
        }

        let buf = read_pipe(rd);
        assert_eq!(buf[0], 1u8);
        assert_eq!(
            std::str::from_utf8(&buf[1..]).unwrap(),
            "daemon exited without signaling readiness"
        );
    }

    #[test]
    fn drop_no_write_after_notify() {
        let (rd, wr) = make_pipe();
        {
            let mut ctx = DaemonContext::new(None, Some(wr), None, None, None, None, None, None);
            ctx.notify_parent().unwrap();
        }
        assert_eq!(read_pipe(rd), vec![0x00]);
    }

    #[test]
    fn debug_format() {
        let ctx = DaemonContext::new(None, None, None, None, None, None, None, None);
        let debug = format!("{:?}", ctx);
        assert!(
            debug.contains("none"),
            "all-None ctx should show 'none': {debug}"
        );
        assert!(
            !debug.contains("Some"),
            "should not contain 'Some': {debug}"
        );
        assert!(
            !debug.contains("None"),
            "should not contain 'None': {debug}"
        );
    }

    #[test]
    fn notify_parent_noop_without_pipe() {
        let mut ctx = DaemonContext::new(None, None, None, None, None, None, None, None);
        assert!(ctx.notify_parent().is_ok());
    }

    #[test]
    fn lockfile_fd_returns_some_with_lockfile() {
        use nix::fcntl::{open, Flock, FlockArg, OFlag};
        use nix::sys::stat::Mode;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let fd = open(
            &path,
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o644),
        )
        .unwrap();
        let flock = Flock::lock(fd, FlockArg::LockExclusiveNonblock).unwrap();
        let ctx = DaemonContext::new(Some(flock), None, None, None, None, None, None, None);
        assert!(ctx.lockfile_fd().is_some());
        // Drop ctx explicitly to release the lock before tempdir cleanup
        drop(ctx);
    }

    #[test]
    fn lockfile_fd_returns_none_without_lockfile() {
        let ctx = DaemonContext::new(None, None, None, None, None, None, None, None);
        assert!(ctx.lockfile_fd().is_none());
    }

    #[test]
    fn drop_privileges_noop_without_user_or_group() {
        let mut ctx = DaemonContext::new(None, None, None, None, None, None, None, None);
        assert!(ctx.drop_privileges().is_ok());
    }

    #[test]
    fn chown_paths_noop_without_user_or_group() {
        let mut ctx = DaemonContext::new(None, None, None, None, None, None, None, None);
        assert!(ctx.chown_paths().is_ok());
    }

    #[test]
    fn drop_privileges_user_not_found() {
        if std::env::var("CI").is_ok() {
            return; // NSS lookups for nonexistent users can hang in CI
        }
        let mut ctx = DaemonContext::new(
            None,
            None,
            None,
            None,
            None,
            None,
            Some("nonexistent_daemonize_test_user_xyz".into()),
            None,
        );
        let result = ctx.drop_privileges();
        assert!(matches!(
            result,
            Err(crate::DaemonizeError::UserNotFound(_))
        ));
    }

    #[test]
    fn drop_privileges_group_not_found() {
        if std::env::var("CI").is_ok() {
            return; // NSS lookups for nonexistent groups can hang in CI
        }
        let mut ctx = DaemonContext::new(
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("nonexistent_daemonize_test_group_xyz".into()),
        );
        let result = ctx.drop_privileges();
        assert!(matches!(
            result,
            Err(crate::DaemonizeError::GroupNotFound(_))
        ));
    }

    #[test]
    fn resolve_user_numeric() {
        // UID 0 should resolve to root on all Unix systems
        let user = resolve_user("0").unwrap();
        assert_eq!(user.uid.as_raw(), 0);
        assert_eq!(user.name, "root");
    }

    #[test]
    fn resolve_user_name() {
        let user = resolve_user("root").unwrap();
        assert_eq!(user.uid.as_raw(), 0);
    }

    #[test]
    fn resolve_group_gid_numeric() {
        let gid = resolve_group_gid("0").unwrap();
        assert_eq!(gid.as_raw(), 0);
    }

    #[test]
    fn context_stores_config_fields() {
        let ctx = DaemonContext::new(
            None,
            None,
            Some("/var/run/test.pid".into()),
            Some("/var/run/test.lock".into()),
            Some("/var/log/test.out".into()),
            Some("/var/log/test.err".into()),
            Some("nobody".into()),
            Some("nogroup".into()),
        );
        let debug = format!("{:?}", ctx);
        assert!(debug.contains("test.pid"));
        assert!(debug.contains("nobody"));
        assert!(debug.contains("nogroup"));
    }

    #[test]
    fn resolve_user_nonexistent_name() {
        if std::env::var("CI").is_ok() {
            return;
        }
        let result = resolve_user("nonexistent_daemonize_test_user_xyz");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_group_gid_by_name() {
        // "root" on Linux, "wheel" on macOS/BSD — try root first to avoid
        // NSS lookup hangs for nonexistent groups in CI.
        let result = resolve_group_gid("root").or_else(|_| resolve_group_gid("wheel"));
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_group_gid_nonexistent_name() {
        if std::env::var("CI").is_ok() {
            return;
        }
        let result = resolve_group_gid("nonexistent_daemonize_test_group_xyz");
        assert!(matches!(
            result,
            Err(crate::DaemonizeError::GroupNotFound(_))
        ));
    }

    #[test]
    fn resolve_uid_gid_user_only() {
        // User only: should return user's UID and primary GID
        let (uid, gid) = resolve_uid_gid(Some("root"), None).unwrap();
        assert_eq!(uid, 0);
        assert_eq!(gid, 0); // root's primary group
    }

    #[test]
    fn resolve_uid_gid_neither() {
        // Neither: should return u32::MAX for both (no change)
        let (uid, gid) = resolve_uid_gid(None, None).unwrap();
        assert_eq!(uid, u32::MAX);
        assert_eq!(gid, u32::MAX);
    }

    #[test]
    fn resolve_uid_gid_group_only_numeric() {
        // Group only with numeric GID
        let (uid, gid) = resolve_uid_gid(None, Some("0")).unwrap();
        assert_eq!(uid, u32::MAX); // no user change
        assert_eq!(gid, 0);
    }

    #[test]
    fn chown_paths_skips_nonexistent_files() {
        // chown_paths should skip files that don't exist
        let mut ctx = DaemonContext::new(
            None,
            None,
            Some("/nonexistent_daemonize_test_xyz/test.pid".into()),
            None,
            None,
            None,
            Some("root".into()),
            None,
        );
        // Should succeed — nonexistent paths are skipped
        assert!(ctx.chown_paths().is_ok());
    }

    #[test]
    fn chown_paths_idempotent() {
        // Calling chown_paths twice should be safe
        let mut ctx = DaemonContext::new(None, None, None, None, None, None, None, None);
        assert!(ctx.chown_paths().is_ok());
        assert!(ctx.chown_paths().is_ok());
    }

    #[test]
    fn drop_privileges_idempotent_noop() {
        // Calling drop_privileges twice with no user/group should be safe
        let mut ctx = DaemonContext::new(None, None, None, None, None, None, None, None);
        assert!(ctx.drop_privileges().is_ok());
        assert!(ctx.drop_privileges().is_ok());
    }

    #[test]
    fn error_display_group_not_found() {
        let err = crate::DaemonizeError::GroupNotFound("nobody".into());
        assert_eq!(err.to_string(), "group not found: nobody");
    }

    #[test]
    fn error_display_chown_error() {
        let err = crate::DaemonizeError::ChownError("/tmp/foo: permission denied".into());
        assert_eq!(err.to_string(), "chown error: /tmp/foo: permission denied");
    }
}
