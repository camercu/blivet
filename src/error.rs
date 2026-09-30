//! Error types for the daemonization sequence, with `sysexits.h` exit codes.

/// Errors produced during configuration validation or the daemonization sequence.
///
/// Each variant maps to a `sysexits.h` exit code via [`exit_code`](DaemonizeError::exit_code).
/// `ProgramNotFound` and `ExecFailed` are produced only by the CLI binary, never by the library.
///
/// # Payload stability
///
/// `String` payloads are human-readable display text, not a stable API:
/// match on the variant and [`exit_code`](DaemonizeError::exit_code), not
/// on payload contents. Only named struct fields (e.g.
/// [`LockConflict::path`](DaemonizeError::LockConflict),
/// [`Application::code`](DaemonizeError::Application)) are stable.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DaemonizeError {
    /// Bad path, bad env key, path overlap, or other config error.
    #[error("validation error: {0}")]
    ValidationError(String),

    /// CLI-only: program missing or not executable — caught by the pre-fork
    /// path check, or at exec time by `ENOENT` (missing PATH command or script
    /// interpreter) or `EACCES` (program not executable).
    #[error("program not found: {0}")]
    ProgramNotFound(String),

    /// User does not exist at runtime during user switching.
    #[error("user not found: {0}")]
    UserNotFound(String),

    /// Group does not exist at runtime during group switching.
    #[error("group not found: {0}")]
    GroupNotFound(String),

    /// flock already held by another process — typically "the daemon is
    /// already running".
    #[error("lock conflict: {} is already locked by another process", path.display())]
    LockConflict {
        /// The effective lockfile (by default the pidfile itself). Callers
        /// handling already-running can read the running instance's PID from
        /// this path when it is the pidfile.
        path: std::path::PathBuf,
    },

    /// Lockfile cannot be opened.
    #[error("lockfile error: {0}")]
    LockfileError(String),

    /// `fork()` returned an error.
    #[error("fork failed: {0}")]
    ForkFailed(String),

    /// `setsid()` returned an error.
    #[error("setsid failed: {0}")]
    SetsidFailed(String),

    /// `chdir()` failed at runtime after fork.
    #[error("chdir failed: {0}")]
    ChdirFailed(String),

    /// A required system call with no more specific variant failed: creating
    /// the notification pipe, opening `/dev/null` or `dup2` onto it,
    /// `sigaction` (resetting signal dispositions), `sigprocmask` (clearing the
    /// signal mask), or `getrlimit` (querying the fd limit before closing
    /// inherited descriptors).
    #[error("system error: {0}")]
    SystemError(String),

    /// Non-root caller with user switch, or setuid/setgid failure.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// Pidfile cannot be written.
    #[error("pidfile error: {0}")]
    PidfileError(String),

    /// stdout/stderr file cannot be opened, truncated or dup2'd, or, in
    /// foreground mode, fd 1/2 cannot be saved first.
    #[error("output file error: {0}")]
    OutputFileError(String),

    /// chown of pidfile/lockfile/output file failed.
    #[error("chown error: {0}")]
    ChownError(String),

    /// CLI-only: exec of target program failed for a reason other than
    /// `ENOENT`/`EACCES` (which map to [`ProgramNotFound`](Self::ProgramNotFound)).
    #[error("exec failed: {0}")]
    ExecFailed(String),

    /// Writing the readiness byte to the parent notification pipe failed.
    ///
    /// Produced by [`notify_parent`](crate::DaemonContext::notify_parent) when
    /// the pipe write returns an `io::Error`.
    #[error("failed to notify parent: {0}")]
    NotifyFailed(#[source] std::io::Error),

    /// Application-level failure during startup, returned from a
    /// [`daemonize_with`](crate::daemonize_with) init or reported by
    /// the caller (e.g. a socket bind or database connect that failed after
    /// [`daemonize`](crate::daemonize) but before
    /// [`notify_parent`](crate::DaemonContext::notify_parent)).
    ///
    /// Unlike the other variants, this one is meant to be constructed by users
    /// — typically via [`application`](DaemonizeError::application) — so they
    /// can surface their own startup errors to the parent through
    /// [`report_error`](crate::DaemonContext::report_error) with an exit code
    /// of their choosing. The stored `code` is surfaced by
    /// [`exit_code`](DaemonizeError::exit_code), except that 0 is remapped to
    /// `70` (`EX_SOFTWARE`) so it can never alias success.
    #[error("application error: {message}")]
    Application {
        /// Exit code to report to the parent (typically a non-zero `sysexits.h`
        /// value; 0 is treated as `EX_SOFTWARE`).
        code: u8,
        /// Human-readable description of the failure.
        message: String,
    },
}

impl DaemonizeError {
    /// Constructs an [`Application`](DaemonizeError::Application) error with a
    /// caller-chosen exit `code` and `message`.
    ///
    /// Return it from a [`daemonize_with`](crate::daemonize_with) init, or use
    /// it to report a failure after the daemonize call, before
    /// [`notify_parent`](crate::DaemonContext::notify_parent), to the parent
    /// process via [`report_error`](crate::DaemonContext::report_error):
    ///
    /// ```no_run
    /// # use blivet::DaemonizeError;
    /// # fn bind() -> std::io::Result<()> { Ok(()) }
    /// // `75` is EX_TEMPFAIL; pick whichever sysexits code fits the failure.
    /// if let Err(e) = bind() {
    ///     let err = DaemonizeError::application(75, format!("bind failed: {e}"));
    ///     // ctx.report_error(&err);
    ///     # let _ = err;
    /// }
    /// ```
    pub fn application(code: u8, message: impl Into<String>) -> Self {
        DaemonizeError::Application {
            code,
            message: message.into(),
        }
    }

    /// Returns the `sysexits.h` exit code for this error variant.
    ///
    /// Always non-zero, so it is safe to pass to `std::process::exit`: a
    /// reported error can never be mistaken for success. A caller-supplied
    /// [`Application`](DaemonizeError::Application) code of 0 is remapped to
    /// `70` (`EX_SOFTWARE`).
    pub fn exit_code(&self) -> u8 {
        match self {
            DaemonizeError::ValidationError(_) => 64,  // EX_USAGE
            DaemonizeError::ProgramNotFound(_) => 66,  // EX_NOINPUT
            DaemonizeError::UserNotFound(_) => 67,     // EX_NOUSER
            DaemonizeError::GroupNotFound(_) => 67,    // EX_NOUSER
            DaemonizeError::LockConflict { .. } => 69, // EX_UNAVAILABLE
            DaemonizeError::LockfileError(_) => 73,    // EX_CANTCREAT
            DaemonizeError::ForkFailed(_) => 71,       // EX_OSERR
            DaemonizeError::SetsidFailed(_) => 71,     // EX_OSERR
            DaemonizeError::ChdirFailed(_) => 71,      // EX_OSERR
            DaemonizeError::SystemError(_) => 71,      // EX_OSERR
            DaemonizeError::PermissionDenied(_) => 77, // EX_NOPERM
            DaemonizeError::PidfileError(_) => 73,     // EX_CANTCREAT
            DaemonizeError::OutputFileError(_) => 73,  // EX_CANTCREAT
            DaemonizeError::ChownError(_) => 73,       // EX_CANTCREAT
            DaemonizeError::ExecFailed(_) => 71,       // EX_OSERR
            DaemonizeError::NotifyFailed(_) => 71,     // EX_OSERR
            // Caller-chosen, but never 0: that would alias success.
            DaemonizeError::Application { code: 0, .. } => 70, // EX_SOFTWARE
            DaemonizeError::Application { code, .. } => *code,
        }
    }
}

/// Gives a failed `Result` the exit code the launcher reports, for `?` inside a
/// [`daemonize_with`](crate::daemonize_with) init.
///
/// ```no_run
/// # fn main() -> Result<(), blivet::DaemonizeError> {
/// use blivet::{daemonize_with, DaemonConfig, WithExitCode};
///
/// let config = DaemonConfig::new();
/// let (mut ctx, listener) = daemonize_with(&config, || {
///     // 71 is EX_OSERR: the launcher exits 71 and prints the bind error.
///     std::net::TcpListener::bind("0.0.0.0:80").with_exit_code(71)
/// })?;
/// # drop(listener);
/// ctx.notify_parent()?;
/// # Ok(())
/// # }
/// ```
pub trait WithExitCode<T> {
    /// Maps the error to [`DaemonizeError::Application`] with `code` and the
    /// error's `Display` text. A `code` of 0 reports as 70, as
    /// [`application`](DaemonizeError::application) does.
    fn with_exit_code(self, code: u8) -> Result<T, DaemonizeError>;
}

impl<T, E: std::fmt::Display> WithExitCode<T> for Result<T, E> {
    fn with_exit_code(self, code: u8) -> Result<T, DaemonizeError> {
        self.map_err(|e| DaemonizeError::application(code, e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Covers: R156
    #[test]
    fn with_exit_code_turns_any_error_into_an_application_error() {
        let failed: Result<(), std::io::Error> =
            Err(std::io::Error::from(std::io::ErrorKind::AddrInUse));

        let err = failed.with_exit_code(71).unwrap_err();

        assert_eq!(err.exit_code(), 71);
        assert_eq!(err.to_string(), "application error: address in use");
    }

    #[test]
    fn with_exit_code_passes_a_value_through() {
        let ok: Result<u8, std::io::Error> = Ok(7);
        assert_eq!(ok.with_exit_code(71).unwrap(), 7);
    }
}
