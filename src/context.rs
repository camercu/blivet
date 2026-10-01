//! Post-daemonization context: parent notification, lockfile management,
//! and pidfile cleanup.

use std::ffi::{CStr, CString};
use std::fmt;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;

use nix::fcntl::Flock;

use crate::config::DaemonConfig;
use crate::error::DaemonizeError;
use crate::notify::NotifyPipe;

/// Context returned by a successful daemonization.
///
/// Returned by [`daemonize`](crate::daemonize) and
/// [`daemonize_with`](crate::daemonize_with) in the daemon, already running as
/// the configured user and group. Holds the lockfile (if configured) and the
/// notification pipe's write end.
///
/// Dropping this without calling [`notify_parent`](DaemonContext::notify_parent)
/// writes a failure message to the notification pipe, causing the parent to
/// exit non-zero. Ending without dropping it, because a signal killed the
/// daemon or it called [`std::process::exit`], closes the pipe with no
/// message, which the parent also reads as a failure. Only
/// [`exec`](DaemonContext::exec) tells the parent to read that close as
/// success. When [`cleanup_on_drop`](crate::DaemonConfig::cleanup_on_drop) is
/// `true` (the default), dropping also removes the pidfile from disk.
///
/// **Signal caveat:** `Drop` does not run when the process is killed by a
/// signal. To clean up the pidfile on `SIGTERM`/`SIGINT`, either call
/// [`cleanup_on_term_signals`](DaemonContext::cleanup_on_term_signals) once
/// (built-in handlers that unlink and re-raise), or install a signal handler
/// that exits the main loop cleanly so this context can drop — see the
/// [README](https://github.com/camercu/blivet#pidfile-cleanup-on-signals) for an example
/// using [`signal_hook`](https://docs.rs/signal-hook).
///
/// The lock is released when this value is dropped.
///
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
            .field(
                "lockfile_path",
                &OptFmt(&self.config.effective_lockfile().cloned()),
            )
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
    /// lock is held (no lockfile configured or derived, or locking disabled
    /// via [`DaemonConfig::no_lockfile`](crate::DaemonConfig::no_lockfile)).
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
    /// on signal termination, call
    /// [`cleanup_on_term_signals`](Self::cleanup_on_term_signals) once, or
    /// install a signal handler (e.g., with
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
    /// One approach per signal: this replaces the signal's disposition, so it
    /// refuses a signal that already has a handler (one registered with
    /// [`signal_hook`](https://docs.rs/signal-hook), say) or is ignored. It also
    /// refuses a signal whose default action does not end the process, such as
    /// `SIGCHLD`: the handler re-raises into that default, so it would remove
    /// the pidfile of a daemon that keeps running. `SIGUSR1` and the other
    /// terminating signals are accepted.
    ///
    /// Call this before any other thread installs signal handlers. The check
    /// and the install are separate steps, so a handler another thread
    /// registers between them is replaced. For a
    /// graceful shutdown, run your own signal loop instead and let this context
    /// drop, or call [`cleanup`](Self::cleanup) — see the
    /// [README](https://github.com/camercu/blivet#pidfile-cleanup-on-signals).
    /// A handler this method installed earlier may be replaced.
    ///
    /// Opt-in, and **library-only**: it has no effect for the `daemonize` CLI,
    /// whose `exec` of the target program resets all custom handlers to their
    /// default disposition. A process that `exec`s must clean up its own
    /// pidfile.
    ///
    /// No-op if no pidfile is configured or `signals` is empty.
    ///
    /// Call this once. Each call leaks a small, bounded copy of the pidfile
    /// path — kept live forever so the async-signal-safe handler can read it
    /// without touching freed memory — so repeated calls (e.g. in a loop) leak
    /// the prior copy each time.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonizeError::ValidationError`] if the pidfile path contains
    /// a NUL byte, if one of `signals` already has a handler other than this
    /// method's or is ignored (checked before anything changes), or if
    /// installing a handler fails — most likely `EINVAL` from
    /// passing a signal that cannot be caught (`SIGKILL`, `SIGSTOP`) or an
    /// invalid signal number. The error names the failing signal, and a failed
    /// install is all-or-nothing: handlers already installed for signals
    /// earlier in the slice are removed again, so an `Err` leaves the process
    /// exactly as it was.
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
        crate::signals::install_pidfile_cleanup_signals(&c_path, signals).map_err(|e| {
            DaemonizeError::ValidationError(format!("failed to install signal handler: {e}"))
        })
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
    /// writing to the pipe fails — e.g. the parent was killed before the
    /// daemon signalled (this error is observable because daemonization
    /// preserves the Rust runtime's ignored SIGPIPE; see
    /// [Signals](crate#signals)). Returning `DaemonizeError` — rather than a
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

    /// Replaces the daemon with `program`, as the `daemonize` CLI does.
    ///
    /// Before `execvp`, it clears close-on-exec on the lockfile so the program
    /// keeps the lock, restores the default `SIGPIPE` disposition (daemonizing
    /// keeps the Rust runtime's ignored one, which would survive the exec),
    /// and tells the launcher an exec is pending. A successful exec closes the
    /// notification pipe and the launcher exits 0. `args` is the full argv,
    /// `args[0]` included; `execvp` searches `PATH` for a bare `program`.
    ///
    /// Never returns. If the exec fails, this reports the error like
    /// [`report_error`](Self::report_error): `ENOENT` and `EACCES` as
    /// [`ProgramNotFound`](DaemonizeError::ProgramNotFound) (exit 66), any
    /// other errno as [`ExecFailed`](DaemonizeError::ExecFailed) (exit 71).
    pub fn exec<S: AsRef<CStr>>(mut self, program: &CStr, args: &[S]) -> ! {
        if let Some(lockfile) = self.lockfile_fd() {
            use nix::fcntl::{fcntl, FcntlArg, FdFlag};
            if let Err(e) = fcntl(lockfile, FcntlArg::F_SETFD(FdFlag::empty())) {
                self.report_error(&DaemonizeError::ExecFailed(format!(
                    "clear close-on-exec on the lockfile: {e}"
                )));
            }
        }
        // The marker goes out while SIGPIPE is still ignored, and a broken pipe
        // there means only that the launcher is gone: run the program anyway
        // (R157). Restoring the default first would let that write kill us.
        if let Some(pipe) = self.notify_pipe.as_mut() {
            match pipe.signal_exec_pending() {
                Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
                    self.report_error(&DaemonizeError::NotifyFailed(e));
                }
                _ => {}
            }
        }
        crate::signals::restore_default_sigpipe();
        let Err(errno) = nix::unistd::execvp(program, args);
        crate::signals::ignore_sigpipe();
        let message = format!("exec {}: {errno}", program.to_string_lossy());
        self.report_error(&exec_error(errno, message))
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
        crate::sys::raw_exit(code as i32)
    }

    /// Removes the pidfile, signals the error to the parent, and returns the
    /// exit code — everything [`report_error`](Self::report_error) does except
    /// the terminal `_exit`. Split out so the observable sequence is testable
    /// in-process (calling `report_error` directly would kill the test).
    ///
    /// **Order matters**, and [`cleanup_then_signal`](Self::cleanup_then_signal)
    /// is where it is decided: cleanup runs *before* the parent is signaled.
    /// The parent unblocking is the synchronization point a caller waits on
    /// (e.g. the shell that ran `daemonize`, or a test observing the process
    /// exit), so any side effect that must be visible by then has to happen
    /// first. Signaling before cleanup let an observer see the parent exit
    /// while the pidfile was still being removed — a race.
    fn cleanup_and_signal_error(&mut self, err: &DaemonizeError) -> u8 {
        self.cleanup_then_signal(|pipe| pipe.signal_error(err));
        err.exit_code()
    }

    /// Removes the pidfile, then tells the parent what happened.
    ///
    /// Both paths that write to the notification pipe without a successful
    /// `notify_parent` — [`report_error`](Self::report_error) and [`Drop`] —
    /// go through here, so there is one ordering to get right instead of two
    /// that can drift apart. They did drift: `Drop` signalled first for long
    /// enough that a parent could wake to a pidfile still on disk, ten lines
    /// below the comment forbidding exactly that.
    fn cleanup_then_signal(&mut self, signal: impl FnOnce(NotifyPipe)) {
        if self.config.cleanup_on_drop {
            self.cleanup();
        }
        if let Some(pipe) = self.notify_pipe.take() {
            signal(pipe);
        }
    }

    /// Reports an application-level failure to the parent process and exits.
    ///
    /// Convenience wrapper over [`report_error`](Self::report_error) for the
    /// common case of surfacing your own startup error (e.g. a socket bind or
    /// database connect that failed during startup) without
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
    /// # let mut ctx = blivet::daemonize(&config)?;
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
        self.cleanup_then_signal(NotifyPipe::signal_unnotified);
    }
}

/// Maps an `execvp` errno to the reported error. `ENOENT` (program or script
/// interpreter missing) and `EACCES` (program not executable) both become
/// [`DaemonizeError::ProgramNotFound`], matching the CLI's pre-fork path check,
/// so a missing or unusable program is exit 66 either way. Every other errno is
/// a genuine OS-level exec failure ([`DaemonizeError::ExecFailed`], exit 71)
/// (R130).
fn exec_error(errno: nix::errno::Errno, message: String) -> DaemonizeError {
    use nix::errno::Errno;
    match errno {
        Errno::ENOENT | Errno::EACCES => DaemonizeError::ProgramNotFound(message),
        _ => DaemonizeError::ExecFailed(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Covers: R130
    #[test]
    fn exec_enoent_maps_to_program_not_found() {
        assert!(matches!(
            exec_error(nix::errno::Errno::ENOENT, "m".into()),
            DaemonizeError::ProgramNotFound(_)
        ));
    }

    // Covers: R130
    #[test]
    fn exec_eacces_maps_to_program_not_found() {
        // A missing-or-unusable program is one exit code (66) in both path and
        // bare form; EACCES is the bare-form counterpart to the path-form X_OK
        // check.
        assert!(matches!(
            exec_error(nix::errno::Errno::EACCES, "m".into()),
            DaemonizeError::ProgramNotFound(_)
        ));
    }

    // Covers: R130
    #[test]
    fn exec_other_errno_stays_exec_failed() {
        // Anything that is not "program missing/unusable" is a genuine OS error
        // (exit 71). E2BIG stands in for that class; it is not integration-
        // reachable, hence the unit test.
        assert!(matches!(
            exec_error(nix::errno::Errno::E2BIG, "m".into()),
            DaemonizeError::ExecFailed(_)
        ));
    }

    use crate::test_support::{make_pipe, read_pipe};
    use std::io::Read;

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

    // Covers: R157
    //
    // A launcher that is gone must not stop the hand-off: the daemon runs the
    // program anyway. Writing the exec marker into a pipe with no reader, after
    // SIGPIPE was restored to its default, killed the daemon instead.
    #[test]
    fn exec_runs_the_program_when_the_launcher_is_gone() {
        const MARKER: &str = "__BLIVET_EXEC_NO_LAUNCHER";
        const NAME: &str = "context::tests::exec_runs_the_program_when_the_launcher_is_gone";

        if std::env::var(MARKER).is_ok() {
            let (rd, wr) = make_pipe();
            drop(rd);
            let ctx = ctx(&DaemonConfig::new(), None, Some(NotifyPipe::new(wr)));
            ctx.exec(c"sh", &[c"sh", c"-c", c"exit 7"]);
        }

        let output = crate::test_support::rerun_in_subprocess(NAME, MARKER, "1");
        assert_eq!(
            output.status.code(),
            Some(7),
            "the program did not run: {}",
            crate::test_support::subprocess_report(NAME, &output)
        );
    }

    // Covers: R157
    //
    // A failed exec reports into the pipe after SIGPIPE was restored to its
    // default; with the launcher gone that write must not kill the daemon
    // before it exits with the exec's code.
    #[test]
    fn exec_failure_exits_with_its_code_when_the_launcher_is_gone() {
        const MARKER: &str = "__BLIVET_EXEC_FAIL_NO_LAUNCHER";
        const NAME: &str =
            "context::tests::exec_failure_exits_with_its_code_when_the_launcher_is_gone";

        if std::env::var(MARKER).is_ok() {
            let (rd, wr) = make_pipe();
            drop(rd);
            let ctx = ctx(&DaemonConfig::new(), None, Some(NotifyPipe::new(wr)));
            ctx.exec(c"/nonexistent_blivet_program", &[c"x"]);
        }

        let output = crate::test_support::rerun_in_subprocess(NAME, MARKER, "1");
        assert_eq!(
            output.status.code(),
            Some(66),
            "{}",
            crate::test_support::subprocess_report(NAME, &output)
        );
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
        assert_eq!(read_pipe(rd), crate::notify::SUCCESS);
    }

    #[test]
    fn notify_parent_or_report_writes_success_byte() {
        let (rd, wr) = make_pipe();
        let mut ctx = ctx(&DaemonConfig::new(), None, Some(NotifyPipe::new(wr)));
        ctx.notify_parent_or_report(); // success path returns normally
        assert_eq!(read_pipe(rd), crate::notify::SUCCESS);
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
        crate::test_support::run_in_subprocess(
            "context::tests::cleanup_on_signals_uncatchable_signal_rejected_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn cleanup_on_signals_uncatchable_signal_rejected_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DaemonConfig::new();
        cfg.pidfile(dir.path().join("x.pid"));
        // SIGKILL cannot be caught -> sigaction EINVAL -> ValidationError.
        assert!(matches!(
            ctx(&cfg, None, None).cleanup_on_signals(&[libc::SIGKILL]),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    // Covers: R149
    #[test]
    fn cleanup_on_signals_refuses_a_signal_that_already_has_a_handler() {
        crate::test_support::run_in_subprocess(
            "context::tests::cleanup_on_signals_refuses_a_signal_that_already_has_a_handler_subprocess",
        );
    }

    /// Another library's handler (signal-hook's, say) on one signal of the
    /// slice: the call is refused, names that signal, and changes nothing,
    /// not even the signals earlier in the slice.
    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn cleanup_on_signals_refuses_a_signal_that_already_has_a_handler_subprocess() {
        use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

        if !crate::test_support::is_subprocess() {
            return;
        }
        extern "C" fn theirs(_: libc::c_int) {}
        let handler = SigAction::new(
            SigHandler::Handler(theirs),
            SaFlags::empty(),
            SigSet::empty(),
        );
        unsafe { sigaction(Signal::SIGUSR2, &handler) }.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DaemonConfig::new();
        cfg.pidfile(dir.path().join("x.pid"));

        let result = ctx(&cfg, None, None).cleanup_on_signals(&[libc::SIGUSR1, libc::SIGUSR2]);

        let query = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        let usr1 = unsafe { sigaction(Signal::SIGUSR1, &query) }.unwrap();
        let usr2 = unsafe { sigaction(Signal::SIGUSR2, &handler) }.unwrap();
        assert!(
            matches!(&result, Err(DaemonizeError::ValidationError(m)) if m.contains(&libc::SIGUSR2.to_string())),
            "{result:?}"
        );
        assert!(
            matches!(usr1.handler(), SigHandler::SigDfl),
            "SIGUSR1 was changed"
        );
        assert!(
            matches!(usr2.handler(), SigHandler::Handler(_)),
            "SIGUSR2 lost its handler"
        );
    }

    // Covers: R149
    #[test]
    fn cleanup_on_signals_refuses_an_ignored_signal() {
        crate::test_support::run_in_subprocess(
            "context::tests::cleanup_on_signals_refuses_an_ignored_signal_subprocess",
        );
    }

    /// An ignored signal is a choice too: installing over it would make the
    /// signal kill the process.
    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn cleanup_on_signals_refuses_an_ignored_signal_subprocess() {
        use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

        if !crate::test_support::is_subprocess() {
            return;
        }
        let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        unsafe { sigaction(Signal::SIGHUP, &ignore) }.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DaemonConfig::new();
        cfg.pidfile(dir.path().join("x.pid"));

        let result = ctx(&cfg, None, None).cleanup_on_signals(&[libc::SIGHUP]);

        assert!(
            matches!(&result, Err(DaemonizeError::ValidationError(m)) if m.contains(&libc::SIGHUP.to_string())),
            "{result:?}"
        );
    }

    // Covers: R149
    #[test]
    fn cleanup_on_signals_may_replace_its_own_handler() {
        crate::test_support::run_in_subprocess(
            "context::tests::cleanup_on_signals_may_replace_its_own_handler_subprocess",
        );
    }

    // Covers: R151
    #[test]
    fn cleanup_on_signals_refuses_a_signal_that_does_not_terminate() {
        crate::test_support::run_in_subprocess(
            "context::tests::cleanup_on_signals_refuses_a_signal_that_does_not_terminate_subprocess",
        );
    }

    /// SIGCHLD is ignored by default: a handler that unlinks the pidfile and
    /// re-raises would delete a running daemon's pidfile at its first child
    /// exit, and the daemon would carry on.
    #[test]
    #[ignore]
    fn cleanup_on_signals_refuses_a_signal_that_does_not_terminate_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DaemonConfig::new();
        cfg.pidfile(dir.path().join("x.pid"));

        let result = ctx(&cfg, None, None).cleanup_on_signals(&[libc::SIGCHLD]);

        assert!(
            matches!(&result, Err(DaemonizeError::ValidationError(m)) if m.contains(&libc::SIGCHLD.to_string())),
            "{result:?}"
        );
    }

    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn cleanup_on_signals_may_replace_its_own_handler_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DaemonConfig::new();
        cfg.pidfile(dir.path().join("x.pid"));
        let ctx = ctx(&cfg, None, None);

        ctx.cleanup_on_signals(&[libc::SIGTERM]).unwrap();
        let again = ctx.cleanup_on_signals(&[libc::SIGTERM]);

        assert!(again.is_ok(), "{again:?}");
        let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a null new action only reads the disposition into `current`.
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &mut current) },
            0
        );
        assert_eq!(
            current.sa_sigaction,
            crate::signals::pidfile_cleanup_handler as extern "C" fn(i32) as usize,
            "SIGTERM is no longer the cleanup handler"
        );
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
            unreachable!("should have been killed by the re-raised SIGTERM");
        }

        use std::os::unix::process::ExitStatusExt;
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("daemon.pid");
        const NAME: &str = "context::tests::cleanup_on_signals_removes_pidfile_on_signal";
        let output = crate::test_support::rerun_in_subprocess(NAME, PIDFILE_ENV, &pidfile);

        assert_eq!(
            output.status.signal(),
            Some(libc::SIGTERM),
            "child should terminate via the re-raised SIGTERM; {}",
            crate::test_support::subprocess_report(NAME, &output)
        );
        assert!(
            !pidfile.exists(),
            "handler should have removed the pidfile before re-raising"
        );
    }

    // The wrapper's whole value is that a caller names no signal constants, so
    // both signals it picks are part of the contract. SIGTERM had a test and
    // SIGINT did not, which left half the set free to be dropped silently —
    // and Ctrl-C is the half a developer meets first.
    #[test]
    fn cleanup_on_term_signals_installs_sigint_handler() {
        const PIDFILE_ENV: &str = "__BLIVET_INT_CLEANUP_PIDFILE";

        if let Ok(path) = std::env::var(PIDFILE_ENV) {
            std::fs::write(&path, "123").unwrap();
            let mut cfg = DaemonConfig::new();
            cfg.pidfile(&path);
            let ctx = ctx(&cfg, None, None);
            ctx.cleanup_on_term_signals().unwrap();
            nix::sys::signal::raise(nix::sys::signal::Signal::SIGINT).unwrap();
            unreachable!("should have been killed by the re-raised SIGINT");
        }

        use std::os::unix::process::ExitStatusExt;
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("daemon.pid");
        const NAME: &str = "context::tests::cleanup_on_term_signals_installs_sigint_handler";
        let output = crate::test_support::rerun_in_subprocess(NAME, PIDFILE_ENV, &pidfile);

        assert_eq!(
            output.status.signal(),
            Some(libc::SIGINT),
            "child should terminate via the re-raised SIGINT; {}",
            crate::test_support::subprocess_report(NAME, &output)
        );
        assert!(
            !pidfile.exists(),
            "handler should have removed the pidfile before re-raising"
        );
    }

    // Same self-spawn pattern as above, through the convenience wrapper: it
    // must actually install handlers for the standard termination signals,
    // not just return Ok.
    #[test]
    fn cleanup_on_term_signals_installs_sigterm_handler() {
        const PIDFILE_ENV: &str = "__BLIVET_TERM_CLEANUP_PIDFILE";

        if let Ok(path) = std::env::var(PIDFILE_ENV) {
            std::fs::write(&path, "123").unwrap();
            let mut cfg = DaemonConfig::new();
            cfg.pidfile(&path);
            let ctx = ctx(&cfg, None, None);
            ctx.cleanup_on_term_signals().unwrap();
            nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).unwrap();
            unreachable!("should have been killed by the re-raised SIGTERM");
        }

        use std::os::unix::process::ExitStatusExt;
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("daemon.pid");
        const NAME: &str = "context::tests::cleanup_on_term_signals_installs_sigterm_handler";
        let output = crate::test_support::rerun_in_subprocess(NAME, PIDFILE_ENV, &pidfile);

        assert_eq!(
            output.status.signal(),
            Some(libc::SIGTERM),
            "child should terminate via the re-raised SIGTERM; {}",
            crate::test_support::subprocess_report(NAME, &output)
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

    // Covers: R143
    #[test]
    fn drop_removes_pidfile_before_signaling_parent() {
        // The parent wakes on the pipe byte, so anything that must be true by
        // then has to happen before the write. An observer that checks the
        // instant the byte arrives can never see the pidfile when the order is
        // right: the unlink happens-before the write, which happens-before the
        // read. `Drop` had the two the wrong way round and nothing noticed,
        // because the sibling test below asserts only that the pidfile is gone
        // once the call has returned, by which time either order has removed
        // it. Both paths share `cleanup_then_signal`, so this pins both for as
        // long as they do.
        for _ in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let pidfile = dir.path().join("daemon.pid");
            std::fs::write(&pidfile, "123").unwrap();

            let (rd, wr) = make_pipe();
            let mut cfg = DaemonConfig::new();
            cfg.pidfile(&pidfile);

            let observer = {
                let pidfile = pidfile.clone();
                std::thread::spawn(move || {
                    let mut byte = [0u8; 1];
                    let mut pipe = std::fs::File::from(rd);
                    pipe.read_exact(&mut byte).expect("the failure byte");
                    pidfile.exists()
                })
            };

            drop(ctx(&cfg, None, Some(NotifyPipe::new(wr))));

            assert!(
                !observer.join().unwrap(),
                "the parent woke to a pidfile that was still on disk"
            );
        }
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
        assert_eq!(read_pipe(rd), crate::notify::SUCCESS);
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
    #[allow(clippy::disallowed_methods)] // a test probe; no stdio slot is in play
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

    // Covers: R54 — dropping the context releases the held flock, so the
    // lockfile can be re-acquired afterward.
    #[test]
    #[allow(clippy::disallowed_methods)] // a test probe; no stdio slot is in play
    fn drop_releases_lockfile() {
        use nix::fcntl::{open, Flock, FlockArg, OFlag};
        use nix::sys::stat::Mode;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let open_lockfile = || {
            open(
                &path,
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o644),
            )
            .unwrap()
        };

        // Hold the lock inside a context.
        let flock = Flock::lock(open_lockfile(), FlockArg::LockExclusiveNonblock).unwrap();
        let ctx = ctx(&DaemonConfig::new(), Some(flock), None);

        // While held, a second exclusive lock on the same file is refused.
        let held = Flock::lock(open_lockfile(), FlockArg::LockExclusiveNonblock);
        assert!(held.is_err(), "lock should be held while context is alive");
        drop(held);

        // Dropping the context releases the lock; it can now be re-acquired.
        drop(ctx);
        let reacquired = Flock::lock(open_lockfile(), FlockArg::LockExclusiveNonblock);
        assert!(
            reacquired.is_ok(),
            "lock should be released after the context is dropped"
        );
    }

    #[test]
    fn lockfile_fd_returns_none_without_lockfile() {
        let ctx = default_ctx();
        assert!(ctx.lockfile_fd().is_none());
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
    fn pidfile_config(pidfile: impl Into<std::path::PathBuf>) -> DaemonConfig {
        let mut config = DaemonConfig::new();
        config.pidfile(pidfile).cleanup_on_drop(false);
        config
    }

    #[test]
    fn error_display_group_not_found() {
        let err = crate::DaemonizeError::GroupNotFound("nobody".into());
        assert_eq!(err.to_string(), "group not found: nobody");
    }

    #[test]
    fn error_display_chown_error() {
        let err = crate::DaemonizeError::ChownError("/a/foo: permission denied".into());
        assert_eq!(err.to_string(), "chown error: /a/foo: permission denied");
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

    // Covers: R19, R76
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
