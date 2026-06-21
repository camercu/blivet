//! Post-daemonization context: parent notification, lockfile management,
//! privilege dropping, and path ownership.

use std::ffi::CString;
use std::fmt;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use nix::fcntl::Flock;

use crate::config::DaemonConfig;
use crate::error::DaemonizeError;
use crate::identity::ResolvedIdentity;
use crate::notify::NotifyPipe;

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
    notify_pipe: Option<NotifyPipe>,
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
        notify_pipe: Option<NotifyPipe>,
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

    /// Installs handlers that remove the pidfile when `SIGINT` or `SIGTERM` is
    /// delivered, then re-raise so the process still terminates.
    ///
    /// Convenience wrapper over [`cleanup_on_signals`](Self::cleanup_on_signals)
    /// for the common case. This is the supported fix for the fact that
    /// [`cleanup`](Self::cleanup) does **not** run on signal termination
    /// (`Drop` is skipped when a signal kills the process), which otherwise
    /// leaves a stale pidfile after the usual `kill`/Ctrl-C shutdown.
    ///
    /// Opt-in: call once after [`daemonize`](crate::daemonize). No-op if no
    /// pidfile is configured.
    ///
    /// # Errors
    ///
    /// See [`cleanup_on_signals`](Self::cleanup_on_signals).
    pub fn cleanup_on_term_signals(&self) -> Result<(), DaemonizeError> {
        self.cleanup_on_signals(&[libc::SIGINT, libc::SIGTERM])
    }

    /// Installs async-signal-safe handlers that remove the pidfile when any of
    /// `signals` is delivered, then restore the default disposition and
    /// re-raise so the process terminates with a status reflecting the signal.
    ///
    /// `signals` are raw signal numbers (e.g. `15` for `SIGTERM`, or
    /// `libc::SIGTERM` if you depend on `libc`). For the usual termination
    /// signals prefer [`cleanup_on_term_signals`](Self::cleanup_on_term_signals),
    /// which needs no signal constants.
    ///
    /// The handler does nothing but `unlink` the pidfile and re-raise, so it is
    /// safe to run from signal context. Only the pidfile is removed; standalone
    /// lockfiles are left on disk (the flock releases when the process exits).
    ///
    /// Opt-in, and **library-only**: it has no effect for the `daemonize` CLI,
    /// whose `exec` of the target program resets all custom handlers to their
    /// default disposition. A process that `exec`s must clean up its own
    /// pidfile.
    ///
    /// No-op if no pidfile is configured or `signals` is empty.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonizeError::ValidationError`] if the pidfile path contains
    /// a NUL byte, or if installing a handler fails — most likely `EINVAL` from
    /// passing a signal that cannot be caught (`SIGKILL`, `SIGSTOP`) or an
    /// invalid signal number.
    pub fn cleanup_on_signals(&self, signals: &[i32]) -> Result<(), DaemonizeError> {
        let Some(ref pidfile) = self.config.pidfile else {
            return Ok(());
        };
        if signals.is_empty() {
            return Ok(());
        }
        let c_path = CString::new(pidfile.as_os_str().as_bytes()).map_err(|_| {
            DaemonizeError::ValidationError("pidfile path contains NUL byte".into())
        })?;
        crate::unsafe_ops::install_pidfile_cleanup_signals(&c_path, signals).map_err(|e| {
            DaemonizeError::ValidationError(format!("failed to install signal handler: {e}"))
        })
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
    /// Returns [`DaemonizeError::NotifyFailed`] (exit code 71, `EX_OSERR`) if
    /// writing to the pipe fails. Returning `DaemonizeError` — rather than a
    /// bare `io::Error` — lets a `fn run() -> Result<(), DaemonizeError>` use
    /// `?` here without wrapping, and preserves the exit code via
    /// [`exit_code`](DaemonizeError::exit_code).
    #[must_use = "the parent process blocks until notified; ignoring this Result may leave it waiting"]
    pub fn notify_parent(&mut self) -> Result<(), DaemonizeError> {
        if let Some(pipe) = self.notify_pipe.take() {
            pipe.signal_ready().map_err(DaemonizeError::NotifyFailed)?;
        }
        Ok(())
    }

    /// Signals readiness like [`notify_parent`](Self::notify_parent), but on
    /// failure cleans up and `_exit`s with the `NotifyFailed` code (71) instead
    /// of returning a `Result`.
    ///
    /// Useful when there is nothing sensible to do on a notify failure but
    /// abort — it never returns on error, so the caller need not thread a
    /// `Result` through `main`. On success it returns normally.
    ///
    /// Note: a failed readiness write has already consumed the notification
    /// pipe (and usually means the parent is gone), so the failure is surfaced
    /// via the exit status and pidfile cleanup, not a message to the parent.
    pub fn notify_parent_or_report(&mut self) {
        if let Some(pipe) = self.notify_pipe.take() {
            if let Err(e) = pipe.signal_ready() {
                self.report_error(&DaemonizeError::NotifyFailed(e));
            }
        }
    }

    /// Reports an error to the parent process and exits.
    ///
    /// Removes the pidfile, then writes the error's exit code byte followed by
    /// the `Display` message to the notification pipe, then calls `_exit()`.
    /// The parent reads this, prints the message to stderr, and exits with the
    /// code.
    ///
    /// Uses `libc::_exit` rather than `std::process::exit` to avoid running
    /// atexit handlers or flushing stdio buffers inherited from the pre-fork
    /// parent, which could cause double-flush corruption or deadlocks.
    ///
    /// Because `_exit` bypasses [`Drop`], this replicates the drop-time pidfile
    /// cleanup itself (gated on
    /// [`cleanup_on_drop`](crate::DaemonConfig::cleanup_on_drop)): a daemon that
    /// aborts startup must not leave a stale pidfile behind.
    pub fn report_error(&mut self, err: &DaemonizeError) -> ! {
        let code = self.cleanup_and_signal_error(err);
        crate::unsafe_ops::raw_exit(code as i32)
    }

    /// Removes the pidfile, signals the error to the parent, and returns the
    /// exit code — everything [`report_error`](Self::report_error) does except
    /// the terminal `_exit`. Split out so the observable sequence is testable
    /// in-process (calling `report_error` directly would kill the test).
    ///
    /// **Order matters:** cleanup runs *before* the parent is signaled. The
    /// parent unblocking is the synchronization point a caller waits on (e.g.
    /// the shell that ran `daemonize`, or a test observing the process exit),
    /// so any side effect that must be visible by then has to happen first.
    /// Signaling before cleanup let an observer see the parent exit while the
    /// pidfile was still being removed — a race.
    fn cleanup_and_signal_error(&mut self, err: &DaemonizeError) -> u8 {
        if self.config.cleanup_on_drop {
            self.cleanup();
        }
        if let Some(pipe) = self.notify_pipe.take() {
            pipe.signal_error(err);
        }
        err.exit_code()
    }

    /// Reports an application-level failure to the parent process and exits.
    ///
    /// Convenience wrapper over [`report_error`](Self::report_error) for the
    /// common case of surfacing your own startup error (e.g. a socket bind or
    /// database connect that failed in the privileged init window) without
    /// having to construct a [`DaemonizeError`] by hand. Equivalent to
    /// `self.report_error(&DaemonizeError::application(code, message))`.
    ///
    /// `code` is reported to the parent verbatim and used as the process exit
    /// code; pick a `sysexits.h` value that fits the failure (e.g. `71` for
    /// `EX_OSERR`, `75` for `EX_TEMPFAIL`).
    ///
    /// ```no_run
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let config = blivet::DaemonConfig::new();
    /// # let mut ctx = unsafe { blivet::daemonize(&config)? };
    /// let listener = match std::net::TcpListener::bind("0.0.0.0:80") {
    ///     Ok(l) => l,
    ///     Err(e) => ctx.report_error_msg(71, format!("bind failed: {e}")),
    /// };
    /// # let _ = listener;
    /// # ctx.notify_parent()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn report_error_msg(&mut self, code: u8, message: impl Into<String>) -> ! {
        self.report_error(&DaemonizeError::application(code, message))
    }
}

impl Drop for DaemonContext {
    fn drop(&mut self) {
        if let Some(pipe) = self.notify_pipe.take() {
            pipe.signal_unnotified();
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
        notify_pipe: Option<NotifyPipe>,
    ) -> DaemonContext {
        DaemonContext::new(config, lockfile, notify_pipe)
    }

    /// Context with all defaults and no runtime resources.
    fn default_ctx() -> DaemonContext {
        ctx(&DaemonConfig::new(), None, None)
    }

    // Covers: R40
    #[test]
    fn notify_parent_writes_success_byte() {
        let (rd, wr) = make_pipe();
        let mut ctx = ctx(&DaemonConfig::new(), None, Some(NotifyPipe::new(wr)));
        ctx.notify_parent().unwrap();
        assert_eq!(read_pipe(rd), vec![0x00]);
    }

    #[test]
    fn notify_parent_or_report_writes_success_byte() {
        let (rd, wr) = make_pipe();
        let mut ctx = ctx(&DaemonConfig::new(), None, Some(NotifyPipe::new(wr)));
        ctx.notify_parent_or_report(); // success path returns normally
        assert_eq!(read_pipe(rd), vec![0x00]);
    }

    #[test]
    fn cleanup_on_signals_noop_without_pidfile() {
        // No pidfile -> nothing to clean -> Ok and no handler installed
        // (so the test process's SIGTERM disposition is left untouched).
        let dctx = default_ctx();
        assert!(dctx.cleanup_on_signals(&[libc::SIGTERM]).is_ok());
        assert!(dctx.cleanup_on_term_signals().is_ok());
        // Empty signal list is also a no-op.
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DaemonConfig::new();
        cfg.pidfile(dir.path().join("x.pid"));
        assert!(ctx(&cfg, None, None).cleanup_on_signals(&[]).is_ok());
    }

    #[test]
    fn cleanup_on_signals_uncatchable_signal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DaemonConfig::new();
        cfg.pidfile(dir.path().join("x.pid"));
        // SIGKILL cannot be caught -> sigaction EINVAL -> ValidationError.
        assert!(matches!(
            ctx(&cfg, None, None).cleanup_on_signals(&[libc::SIGKILL]),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    // Installs a real SIGTERM handler and raises it, so it must run in its own
    // process: it self-spawns a child (via an env marker) that dies from the
    // signal, then the parent asserts the pidfile was removed by the handler
    // and the child terminated *via* SIGTERM (proving the re-raise).
    #[test]
    fn cleanup_on_signals_removes_pidfile_on_signal() {
        const PIDFILE_ENV: &str = "__BLIVET_CLEANUP_PIDFILE";

        if let Ok(path) = std::env::var(PIDFILE_ENV) {
            std::fs::write(&path, "123").unwrap();
            let mut cfg = DaemonConfig::new();
            cfg.pidfile(&path);
            let ctx = ctx(&cfg, None, None);
            ctx.cleanup_on_signals(&[libc::SIGTERM]).unwrap();
            nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(5));
            unreachable!("should have been killed by the re-raised SIGTERM");
        }

        use std::os::unix::process::ExitStatusExt;
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("daemon.pid");
        let exe = std::env::current_exe().unwrap();
        let status = std::process::Command::new(exe)
            .arg("--exact")
            .arg("context::tests::cleanup_on_signals_removes_pidfile_on_signal")
            .arg("--nocapture")
            .env(PIDFILE_ENV, &pidfile)
            .status()
            .unwrap();

        assert_eq!(
            status.signal(),
            Some(libc::SIGTERM),
            "child should terminate via the re-raised SIGTERM"
        );
        assert!(
            !pidfile.exists(),
            "handler should have removed the pidfile before re-raising"
        );
    }

    #[test]
    fn notify_parent_idempotent() {
        let (_rd, wr) = make_pipe();
        let mut ctx = ctx(&DaemonConfig::new(), None, Some(NotifyPipe::new(wr)));
        ctx.notify_parent().unwrap();
        ctx.notify_parent().unwrap();
    }

    // Covers: R41, R117
    #[test]
    fn drop_writes_failure_when_not_notified() {
        let (rd, wr) = make_pipe();
        {
            let _ctx = ctx(&DaemonConfig::new(), None, Some(NotifyPipe::new(wr)));
        }

        let buf = read_pipe(rd);
        assert_eq!(buf[0], 1u8);
        assert_eq!(
            std::str::from_utf8(&buf[1..]).unwrap(),
            "daemon exited without signaling readiness"
        );
    }

    // Covers: R118
    #[test]
    fn report_error_removes_pidfile_before_signaling_parent() {
        // Regression for a fork race: the pidfile must be gone by the time the
        // parent is signaled, since an observer (shell/test) wakes on that
        // signal. Tested in-process via cleanup_and_signal_error (report_error
        // itself would _exit and kill the test). Real pipe + real tempfile, no
        // mocks: after signaling, the pidfile is already removed.
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("daemon.pid");
        std::fs::write(&pidfile, "123").unwrap();

        let (rd, wr) = make_pipe();
        let mut cfg = DaemonConfig::new();
        cfg.pidfile(&pidfile);
        let mut ctx = ctx(&cfg, None, Some(NotifyPipe::new(wr)));

        let code = ctx.cleanup_and_signal_error(&DaemonizeError::ExecFailed("boom".into()));

        assert_eq!(code, 71, "ExecFailed maps to exit 71");
        assert!(
            !pidfile.exists(),
            "pidfile must be removed before the parent is signaled"
        );
        let buf = read_pipe(rd);
        assert_eq!(buf[0], 71, "error code byte signaled to parent");
        assert_eq!(std::str::from_utf8(&buf[1..]).unwrap(), "exec failed: boom");
    }

    #[test]
    fn report_error_respects_cleanup_on_drop_disabled() {
        // With cleanup_on_drop=false the pidfile is intentionally preserved.
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("daemon.pid");
        std::fs::write(&pidfile, "123").unwrap();

        let (_rd, wr) = make_pipe();
        let mut cfg = DaemonConfig::new();
        cfg.pidfile(&pidfile).cleanup_on_drop(false);
        let mut ctx = ctx(&cfg, None, Some(NotifyPipe::new(wr)));

        ctx.cleanup_and_signal_error(&DaemonizeError::ExecFailed("boom".into()));

        assert!(pidfile.exists(), "cleanup_on_drop=false keeps the pidfile");
    }

    #[test]
    fn drop_no_write_after_notify() {
        let (rd, wr) = make_pipe();
        {
            let mut ctx = ctx(&DaemonConfig::new(), None, Some(NotifyPipe::new(wr)));
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

    // Covers: R62
    #[test]
    fn drop_privileges_noop_without_user_or_group() {
        let mut ctx = default_ctx();
        assert!(ctx.drop_privileges().is_ok());
    }

    // Covers: R65
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

    // Covers: R120
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

    // Covers: R72
    #[test]
    fn cleanup_removes_pidfile() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        std::fs::write(&pidfile, "12345\n").unwrap();

        let mut ctx = ctx(&pidfile_config(&pidfile), None, None);
        ctx.cleanup();
        assert!(!pidfile.exists(), "pidfile should be removed after cleanup");
    }

    // Covers: R73
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

    // Covers: R74
    #[test]
    fn cleanup_ignores_missing_pidfile() {
        let mut ctx = ctx(&pidfile_config("/nonexistent_xyz/test.pid"), None, None);
        ctx.cleanup(); // best-effort, should not panic
    }

    // Covers: R76
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

    // Covers: R77
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

    // Covers: R78
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

    // Covers: R75
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
