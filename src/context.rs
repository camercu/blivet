//! Post-daemonization context: parent notification, lockfile management,
//! privilege dropping, and path ownership.

use std::fmt;
use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;

use nix::fcntl::Flock;

use crate::config::DaemonConfig;
use crate::error::DaemonizeError;
use crate::identity::ResolvedIdentity;

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
/// exit non-zero. When [`cleanup_on_drop`](crate::DaemonConfig::cleanup_on_drop)
/// is `true` (the default), dropping also removes the pidfile from disk.
///
/// **Signal caveat:** `Drop` does not run when the process is killed by a
/// signal. To clean up the pidfile on `SIGTERM`/`SIGINT`, install a signal
/// handler that exits the main loop cleanly so this context can drop (or
/// call [`cleanup()`](DaemonContext::cleanup) explicitly). See the
/// [README](https://github.com/camercu/blivet#pidfile-cleanup) for an example
/// using [`signal_hook`](https://docs.rs/signal-hook).
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
    /// The validated config, carried whole. The single source of truth for
    /// post-daemonization fields (pidfile, user, group, …) rather than
    /// mirroring each one into a parallel field set.
    config: DaemonConfig,
    lockfile: Option<Flock<OwnedFd>>,
    notify_pipe: Option<OwnedFd>,
    cleaned_up: bool,
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
            .field("pidfile", &OptFmt(&self.config.pidfile))
            .field("lockfile_path", &OptFmt(&self.config.lockfile))
            .field("stdout", &OptFmt(&self.config.stdout))
            .field("stderr", &OptFmt(&self.config.stderr))
            .field("user", &OptFmt(&self.config.user))
            .field("group", &OptFmt(&self.config.group))
            .field("cleanup_on_drop", &self.config.cleanup_on_drop)
            .finish()
    }
}

impl DaemonContext {
    /// Builds a context from the validated config plus the two runtime
    /// resources acquired during daemonization: the held lockfile and the
    /// notification pipe's write end. The config-derived fields are cloned
    /// from `config`.
    pub(crate) fn new(
        config: &DaemonConfig,
        lockfile: Option<Flock<OwnedFd>>,
        notify_pipe: Option<OwnedFd>,
    ) -> Self {
        Self {
            config: config.clone(),
            lockfile,
            notify_pipe,
            cleaned_up: false,
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

    /// Sets whether [`cleanup`](DaemonContext::cleanup) runs automatically
    /// when this context is dropped.
    ///
    /// Overrides the value set by
    /// [`DaemonConfig::cleanup_on_drop`](crate::DaemonConfig::cleanup_on_drop).
    pub fn set_cleanup_on_drop(&mut self, cleanup: bool) {
        self.config.cleanup_on_drop = cleanup;
    }

    /// Removes the pidfile from disk (best-effort).
    ///
    /// Only the pidfile is removed. Standalone lockfiles are left on disk
    /// (the flock is released when this context is dropped).
    ///
    /// Errors are silently ignored — the daemon is shutting down and there
    /// is nothing useful to do with them. Safe to call multiple times
    /// (idempotent).
    ///
    /// Runs automatically on drop when
    /// [`cleanup_on_drop`](crate::DaemonConfig::cleanup_on_drop) is `true`
    /// (the default). Note that `Drop` **does not run** when the process is
    /// killed by a signal (`SIGTERM`, `SIGKILL`, etc.). To remove the pidfile
    /// on signal termination, install a signal handler (e.g., with
    /// [`signal_hook`](https://docs.rs/signal-hook)) that exits the main loop
    /// cleanly, allowing this context to drop or calling `cleanup()` explicitly.
    pub fn cleanup(&mut self) {
        if self.cleaned_up {
            return;
        }
        self.cleaned_up = true;

        if let Some(ref path) = self.config.pidfile {
            let _ = std::fs::remove_file(path);
        }
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
        if self.config.user.is_none() && self.config.group.is_none() {
            return Ok(());
        }

        let identity =
            ResolvedIdentity::resolve(self.config.user.as_deref(), self.config.group.as_deref())?;
        let (owner, group) = identity.chown_ids();

        let paths: Vec<&PathBuf> = [
            &self.config.pidfile,
            &self.config.lockfile,
            &self.config.stdout,
            &self.config.stderr,
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
        if self.config.user.is_none() && self.config.group.is_none() {
            return Ok(());
        }

        let identity =
            ResolvedIdentity::resolve(self.config.user.as_deref(), self.config.group.as_deref())?;

        if let Some(info) = identity.user() {
            crate::unsafe_ops::raw_initgroups(&info.cname()?, info.gid.as_raw())
                .map_err(|e| DaemonizeError::PermissionDenied(format!("initgroups: {e}")))?;
        }

        // setgid: explicit group if set, otherwise the user's primary group.
        if let Some(gid) = identity.effective_gid() {
            nix::unistd::setgid(gid)
                .map_err(|e| DaemonizeError::PermissionDenied(format!("setgid: {e}")))?;
        }

        if let Some(info) = identity.user() {
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
            file.write_all(&crate::notify::SUCCESS)?;
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
            let _ = file.write_all(&crate::notify::error_bytes(err));
            let _ = file.flush();
        }
        crate::unsafe_ops::raw_exit(err.exit_code() as i32)
    }
}

impl Drop for DaemonContext {
    fn drop(&mut self) {
        if let Some(fd) = self.notify_pipe.take() {
            let mut file = io::BufWriter::new(std::fs::File::from(fd));
            let _ = file.write_all(&crate::notify::unnotified_bytes());
            let _ = file.flush();
        }
        if self.config.cleanup_on_drop {
            self.cleanup();
        }
    }
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

    /// Build a context directly from a configured `DaemonConfig` plus the
    /// optional runtime resources. Mirrors `DaemonContext::new` without a
    /// parallel field set — tests configure via the real builder.
    fn ctx(
        config: &DaemonConfig,
        lockfile: Option<Flock<OwnedFd>>,
        notify_pipe: Option<OwnedFd>,
    ) -> DaemonContext {
        DaemonContext::new(config, lockfile, notify_pipe)
    }

    /// Context with all defaults and no runtime resources.
    fn default_ctx() -> DaemonContext {
        ctx(&DaemonConfig::new(), None, None)
    }

    #[test]
    fn notify_parent_writes_success_byte() {
        let (rd, wr) = make_pipe();
        let mut ctx = ctx(&DaemonConfig::new(), None, Some(wr));
        ctx.notify_parent().unwrap();
        assert_eq!(read_pipe(rd), vec![0x00]);
    }

    #[test]
    fn notify_parent_idempotent() {
        let (_rd, wr) = make_pipe();
        let mut ctx = ctx(&DaemonConfig::new(), None, Some(wr));
        ctx.notify_parent().unwrap();
        ctx.notify_parent().unwrap();
    }

    #[test]
    fn drop_writes_failure_when_not_notified() {
        let (rd, wr) = make_pipe();
        {
            let _ctx = ctx(&DaemonConfig::new(), None, Some(wr));
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
            let mut ctx = ctx(&DaemonConfig::new(), None, Some(wr));
            ctx.notify_parent().unwrap();
        }
        assert_eq!(read_pipe(rd), vec![0x00]);
    }

    #[test]
    fn debug_format() {
        let ctx = default_ctx();
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
        let mut ctx = default_ctx();
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
        let ctx = ctx(&DaemonConfig::new(), Some(flock), None);
        assert!(ctx.lockfile_fd().is_some());
        drop(ctx);
    }

    #[test]
    fn lockfile_fd_returns_none_without_lockfile() {
        let ctx = default_ctx();
        assert!(ctx.lockfile_fd().is_none());
    }

    #[test]
    fn drop_privileges_noop_without_user_or_group() {
        let mut ctx = default_ctx();
        assert!(ctx.drop_privileges().is_ok());
    }

    #[test]
    fn chown_paths_noop_without_user_or_group() {
        let mut ctx = default_ctx();
        assert!(ctx.chown_paths().is_ok());
    }

    #[test]
    fn drop_privileges_user_not_found() {
        if std::env::var("CI").is_ok() {
            return; // NSS lookups for nonexistent users can hang in CI
        }
        let mut config = DaemonConfig::new();
        config.user("nonexistent_daemonize_test_user_xyz");
        let mut ctx = ctx(&config, None, None);
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
        let mut config = DaemonConfig::new();
        config.group("nonexistent_daemonize_test_group_xyz");
        let mut ctx = ctx(&config, None, None);
        let result = ctx.drop_privileges();
        assert!(matches!(
            result,
            Err(crate::DaemonizeError::GroupNotFound(_))
        ));
    }

    #[test]
    fn context_stores_config_fields() {
        let mut config = DaemonConfig::new();
        config
            .pidfile("/var/run/test.pid")
            .lockfile("/var/run/test.lock")
            .stdout("/var/log/test.out")
            .stderr("/var/log/test.err")
            .user("nobody")
            .group("nogroup");
        let ctx = ctx(&config, None, None);
        let debug = format!("{:?}", ctx);
        assert!(debug.contains("test.pid"));
        assert!(debug.contains("nobody"));
        assert!(debug.contains("nogroup"));
    }

    /// Config carrying a pidfile, with `cleanup_on_drop` disabled so tests
    /// control cleanup explicitly.
    fn pidfile_config(pidfile: impl Into<PathBuf>) -> DaemonConfig {
        let mut config = DaemonConfig::new();
        config.pidfile(pidfile).cleanup_on_drop(false);
        config
    }

    #[test]
    fn chown_paths_skips_nonexistent_files() {
        let mut config = pidfile_config("/nonexistent_daemonize_test_xyz/test.pid");
        config.user("root");
        let mut ctx = ctx(&config, None, None);
        assert!(ctx.chown_paths().is_ok());
    }

    #[test]
    fn chown_paths_idempotent() {
        let mut ctx = default_ctx();
        assert!(ctx.chown_paths().is_ok());
        assert!(ctx.chown_paths().is_ok());
    }

    #[test]
    fn drop_privileges_idempotent_noop() {
        let mut ctx = default_ctx();
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

    // --- cleanup ---

    #[test]
    fn cleanup_removes_pidfile() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        std::fs::write(&pidfile, "12345\n").unwrap();

        let mut ctx = ctx(&pidfile_config(&pidfile), None, None);
        ctx.cleanup();
        assert!(!pidfile.exists(), "pidfile should be removed after cleanup");
    }

    #[test]
    fn cleanup_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        std::fs::write(&pidfile, "12345\n").unwrap();

        let mut ctx = ctx(&pidfile_config(&pidfile), None, None);
        ctx.cleanup();
        ctx.cleanup(); // second call should not panic
        assert!(!pidfile.exists());
    }

    #[test]
    fn cleanup_noop_without_pidfile() {
        let mut config = DaemonConfig::new();
        config.cleanup_on_drop(false);
        let mut ctx = ctx(&config, None, None);
        ctx.cleanup(); // should not panic
    }

    #[test]
    fn cleanup_ignores_missing_pidfile() {
        let mut ctx = ctx(&pidfile_config("/nonexistent_xyz/test.pid"), None, None);
        ctx.cleanup(); // best-effort, should not panic
    }

    #[test]
    fn drop_cleans_up_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        std::fs::write(&pidfile, "12345\n").unwrap();

        {
            let mut config = DaemonConfig::new();
            config.pidfile(&pidfile); // cleanup_on_drop defaults to true
            let _ctx = ctx(&config, None, None);
        }
        assert!(!pidfile.exists(), "pidfile should be removed on drop");
    }

    #[test]
    fn drop_skips_cleanup_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        std::fs::write(&pidfile, "12345\n").unwrap();

        {
            let _ctx = ctx(&pidfile_config(&pidfile), None, None);
        }
        assert!(
            pidfile.exists(),
            "pidfile should survive drop when cleanup_on_drop=false"
        );
    }

    #[test]
    fn set_cleanup_on_drop_overrides_config() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        std::fs::write(&pidfile, "12345\n").unwrap();

        {
            let mut ctx = ctx(&pidfile_config(&pidfile), None, None);
            ctx.set_cleanup_on_drop(true);
        }
        assert!(
            !pidfile.exists(),
            "pidfile should be removed after runtime override"
        );
    }

    #[test]
    fn cleanup_leaves_standalone_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        let lockfile_path = dir.path().join("test.lock");
        std::fs::write(&pidfile, "12345\n").unwrap();
        std::fs::write(&lockfile_path, "").unwrap();

        let mut config = pidfile_config(&pidfile);
        config.lockfile(&lockfile_path);
        let mut ctx = ctx(&config, None, None);
        ctx.cleanup();
        assert!(!pidfile.exists(), "pidfile should be removed");
        assert!(
            lockfile_path.exists(),
            "standalone lockfile should be left on disk"
        );
    }
}
