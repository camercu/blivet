//! Error types for the daemonization sequence, with `sysexits.h` exit codes.

/// Errors produced during configuration validation or the daemonization sequence.
///
/// Each variant maps to a `sysexits.h` exit code via [`exit_code`](DaemonizeError::exit_code).
/// `ProgramNotFound` and `ExecFailed` are produced only by the CLI binary, never by the library.
#[derive(Debug, thiserror::Error)]
pub enum DaemonizeError {
    /// Bad path, bad env key, path overlap, or other config error.
    #[error("{0}")]
    ValidationError(String),

    /// CLI-only: program path missing or not executable.
    #[error("{0}")]
    ProgramNotFound(String),

    /// User does not exist at runtime during user switching.
    #[error("{0}")]
    UserNotFound(String),

    /// flock already held by another process.
    #[error("{0}")]
    LockConflict(String),

    /// Lockfile cannot be opened.
    #[error("{0}")]
    LockfileError(String),

    /// `fork()` returned an error.
    #[error("{0}")]
    ForkFailed(String),

    /// `setsid()` returned an error.
    #[error("{0}")]
    SetsidFailed(String),

    /// `chdir()` failed at runtime after fork.
    #[error("{0}")]
    ChdirFailed(String),

    /// Non-root caller with user switch, or setuid/setgid failure.
    #[error("{0}")]
    PermissionDenied(String),

    /// Pidfile cannot be written.
    #[error("{0}")]
    PidfileError(String),

    /// stdout/stderr file cannot be opened or dup2'd.
    #[error("{0}")]
    OutputFileError(String),

    /// CLI-only: exec of target program failed.
    #[error("{0}")]
    ExecFailed(String),
}

impl DaemonizeError {
    /// Returns the `sysexits.h` exit code for this error variant.
    pub fn exit_code(&self) -> u8 {
        match self {
            DaemonizeError::ValidationError(_) => 64,  // EX_USAGE
            DaemonizeError::ProgramNotFound(_) => 66,  // EX_NOINPUT
            DaemonizeError::UserNotFound(_) => 67,     // EX_NOUSER
            DaemonizeError::LockConflict(_) => 69,     // EX_UNAVAILABLE
            DaemonizeError::LockfileError(_) => 73,    // EX_CANTCREAT
            DaemonizeError::ForkFailed(_) => 71,       // EX_OSERR
            DaemonizeError::SetsidFailed(_) => 71,     // EX_OSERR
            DaemonizeError::ChdirFailed(_) => 71,      // EX_OSERR
            DaemonizeError::PermissionDenied(_) => 77, // EX_NOPERM
            DaemonizeError::PidfileError(_) => 73,     // EX_CANTCREAT
            DaemonizeError::OutputFileError(_) => 73,  // EX_CANTCREAT
            DaemonizeError::ExecFailed(_) => 71,       // EX_OSERR
        }
    }
}
