//! Error types for the daemonization sequence, with `sysexits.h` exit codes.

/// Errors produced during configuration validation or the daemonization sequence.
///
/// Each variant maps to a `sysexits.h` exit code via [`exit_code`](DaemonizeError::exit_code).
/// `ProgramNotFound` and `ExecFailed` are produced only by the CLI binary, never by the library.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DaemonizeError {
    /// Bad path, bad env key, path overlap, or other config error.
    #[error("validation error: {0}")]
    ValidationError(String),

    /// CLI-only: program path missing or not executable.
    #[error("program not found: {0}")]
    ProgramNotFound(String),

    /// User does not exist at runtime during user switching.
    #[error("user not found: {0}")]
    UserNotFound(String),

    /// Group does not exist at runtime during group switching.
    #[error("group not found: {0}")]
    GroupNotFound(String),

    /// flock already held by another process.
    #[error("lock conflict: {0}")]
    LockConflict(String),

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

    /// Non-root caller with user switch, or setuid/setgid failure.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// Pidfile cannot be written.
    #[error("pidfile error: {0}")]
    PidfileError(String),

    /// stdout/stderr file cannot be opened or dup2'd.
    #[error("output file error: {0}")]
    OutputFileError(String),

    /// chown of pidfile/lockfile/output file failed.
    #[error("chown error: {0}")]
    ChownError(String),

    /// CLI-only: exec of target program failed.
    #[error("exec failed: {0}")]
    ExecFailed(String),

    /// Writing the readiness byte to the parent notification pipe failed.
    ///
    /// Produced by [`notify_parent`](crate::DaemonContext::notify_parent) when
    /// the pipe write returns an `io::Error`.
    #[error("failed to notify parent: {0}")]
    NotifyFailed(#[source] std::io::Error),

    /// A user and/or group was configured but
    /// [`drop_privileges`](crate::DaemonContext::drop_privileges) was never
    /// called before the daemon signaled readiness.
    ///
    /// Surfaced by [`notify_parent`](crate::DaemonContext::notify_parent) so a
    /// daemon configured to run as an unprivileged user fails loudly instead of
    /// silently continuing to run with elevated privileges.
    #[error(
        "privileges not dropped: a user/group was configured but \
         drop_privileges() was never called before notifying readiness"
    )]
    PrivilegesNotDropped,

    /// Application-level failure during the privileged init window, reported by
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
    /// Use this to report a failure that happens in the privileged init window
    /// (after [`daemonize`](crate::daemonize), before
    /// [`notify_parent`](crate::DaemonContext::notify_parent)) to the parent
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
            DaemonizeError::ValidationError(_) => 64,   // EX_USAGE
            DaemonizeError::ProgramNotFound(_) => 66,   // EX_NOINPUT
            DaemonizeError::UserNotFound(_) => 67,      // EX_NOUSER
            DaemonizeError::GroupNotFound(_) => 67,     // EX_NOUSER
            DaemonizeError::LockConflict(_) => 69,      // EX_UNAVAILABLE
            DaemonizeError::LockfileError(_) => 73,     // EX_CANTCREAT
            DaemonizeError::ForkFailed(_) => 71,        // EX_OSERR
            DaemonizeError::SetsidFailed(_) => 71,      // EX_OSERR
            DaemonizeError::ChdirFailed(_) => 71,       // EX_OSERR
            DaemonizeError::PermissionDenied(_) => 77,  // EX_NOPERM
            DaemonizeError::PidfileError(_) => 73,      // EX_CANTCREAT
            DaemonizeError::OutputFileError(_) => 73,   // EX_CANTCREAT
            DaemonizeError::ChownError(_) => 73,        // EX_CANTCREAT
            DaemonizeError::ExecFailed(_) => 71,        // EX_OSERR
            DaemonizeError::NotifyFailed(_) => 71,      // EX_OSERR
            DaemonizeError::PrivilegesNotDropped => 70, // EX_SOFTWARE
            // Caller-chosen, but never 0: that would alias success.
            DaemonizeError::Application { code: 0, .. } => 70, // EX_SOFTWARE
            DaemonizeError::Application { code, .. } => *code,
        }
    }
}
