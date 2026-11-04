//! Error types for the daemonization sequence, with `sysexits.h` exit codes.

/// An error from configuration validation or the daemonization sequence:
/// what failed, and what a rollback could not put back.
///
/// [`Display`](std::fmt::Display) and [`source`](std::error::Error::source)
/// are the [`kind`](Self::kind)'s, so an error from a
/// [`daemonize_with`](crate::daemonize_with) init keeps its own message (R154).
/// The [`rollback_failures`](Self::rollback_failures) are separate (R164).
#[derive(Debug)]
pub struct DaemonizeError {
    kind: ErrorKind,
    rollback_failures: Vec<RestoreFailure>,
}

impl DaemonizeError {
    /// Constructs an [`Application`](ErrorKind::Application) error with a
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
        ErrorKind::application(code, message).into()
    }

    /// What failed.
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    /// Each change a failed start could not put back, latest first (R164).
    /// A daemon exits instead of returning this error, and sends these to
    /// its launcher after the message (R160).
    pub fn rollback_failures(&self) -> &[RestoreFailure] {
        &self.rollback_failures
    }

    /// Returns the `sysexits.h` exit code for the [`kind`](Self::kind).
    ///
    /// Always non-zero, so it is safe to pass to `std::process::exit`: a
    /// reported error can never be mistaken for success. A caller-supplied
    /// [`Application`](ErrorKind::Application) code of 0 is remapped to
    /// `70` (`EX_SOFTWARE`).
    pub fn exit_code(&self) -> u8 {
        self.kind.exit_code()
    }

    /// What failed, by value, without the
    /// [`rollback_failures`](Self::rollback_failures).
    pub fn into_kind(self) -> ErrorKind {
        self.kind
    }

    /// The message a launcher prints for this error: the
    /// [`kind`](Self::kind)'s, then one `rollback left` line per
    /// [rollback failure](Self::rollback_failures) (R160, R164).
    pub(crate) fn report(&self) -> Report<'_> {
        self.report_also(&[])
    }

    /// As [`report`](Self::report), with `more` failures after the ones
    /// this error holds.
    pub(crate) fn report_also<'a>(&'a self, more: &'a [RestoreFailure]) -> Report<'a> {
        Report::new(&self.kind, self.rollback_failures.iter().chain(more))
    }

    /// This error, with `failures` after the ones it holds.
    pub(crate) fn with_rollback_failures(
        mut self,
        failures: impl IntoIterator<Item = RestoreFailure>,
    ) -> Self {
        self.rollback_failures.extend(failures);
        self
    }
}

impl From<ErrorKind> for DaemonizeError {
    fn from(kind: ErrorKind) -> Self {
        Self {
            kind,
            rollback_failures: Vec::new(),
        }
    }
}

impl std::fmt::Display for DaemonizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.kind.fmt(f)
    }
}

/// A message for the launcher: a first line, then one `rollback left` line
/// per [rollback failure](RestoreFailure) (R160, R164).
pub(crate) struct Report<'a> {
    head: &'a dyn std::fmt::Display,
    failures: Vec<&'a RestoreFailure>,
}

impl<'a> Report<'a> {
    pub(crate) fn new(
        head: &'a dyn std::fmt::Display,
        failures: impl IntoIterator<Item = &'a RestoreFailure>,
    ) -> Self {
        Self {
            head,
            failures: failures.into_iter().collect(),
        }
    }
}

impl std::fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.head.fmt(f)?;
        for failure in &self.failures {
            write!(f, "\nrollback left {failure}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DaemonizeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.kind.source()
    }
}

/// A change a rollback could not put back (R164).
#[derive(Debug)]
pub struct RestoreFailure {
    state: ChangedState,
    path: Option<std::path::PathBuf>,
    error: std::io::Error,
}

impl RestoreFailure {
    pub(crate) fn new(state: ChangedState, error: impl Into<std::io::Error>) -> Self {
        Self {
            state,
            path: None,
            error: error.into(),
        }
    }

    /// A failure to put back the file at `path`.
    pub(crate) fn at(
        state: ChangedState,
        path: &std::path::Path,
        error: impl Into<std::io::Error>,
    ) -> Self {
        Self {
            path: Some(path.to_path_buf()),
            ..Self::new(state, error)
        }
    }

    /// The state left changed.
    pub fn state(&self) -> ChangedState {
        self.state
    }

    /// The file left changed, for state that is a file on disk.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Why it could not be put back: the restore's error, or the error that
    /// kept the step from saving it.
    pub fn error(&self) -> &std::io::Error {
        &self.error
    }
}

impl std::fmt::Display for RestoreFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(path) => write!(f, "{} {}: {}", self.state, path.display(), self.error),
            None => write!(f, "{}: {}", self.state, self.error),
        }
    }
}

/// No [`source`](std::error::Error::source): the message already names the
/// error, and an error chain would print it twice.
impl std::error::Error for RestoreFailure {}

/// What the sequence changed and a rollback puts back (R164): the
/// process's own state, and files on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChangedState {
    /// Descriptor 0.
    Stdin,
    /// Descriptor 1.
    Stdout,
    /// Descriptor 2.
    Stderr,
    /// The working directory.
    WorkingDirectory,
    /// The signal mask.
    SignalMask,
    /// The signal dispositions.
    SignalDispositions,
    /// The supplementary groups.
    SupplementaryGroups,
    /// The real and effective group of an identity switch whose `setuid`
    /// failed.
    PrimaryGroup,
    /// The pidfile the sequence wrote, still on disk.
    Pidfile,
    /// The lockfile the sequence created, still on disk.
    Lockfile,
    /// A stdout or stderr file the sequence created, still on disk.
    OutputFile,
    /// The owner and group of a file the sequence handed to the target
    /// identity.
    FileOwner,
    /// The environment variables the config set. Left as set once `init`
    /// has run: init may have started threads, and `setenv` is not
    /// thread-safe.
    Environment,
    /// A stdout or stderr file that was there before the start, which the
    /// sequence emptied of its earlier content. The file is the caller's
    /// own, not a leftover: do not delete it.
    EmptiedOutputFile,
}

impl ChangedState {
    /// Whether the change stays after the process exits: a file on disk, not
    /// the process's own state. A program that exits on a failed start need
    /// only report these.
    pub fn outlives_process(self) -> bool {
        match self {
            Self::Stdin
            | Self::Stdout
            | Self::Stderr
            | Self::WorkingDirectory
            | Self::SignalMask
            | Self::SignalDispositions
            | Self::SupplementaryGroups
            | Self::Environment
            | Self::PrimaryGroup => false,
            Self::Pidfile
            | Self::Lockfile
            | Self::OutputFile
            | Self::EmptiedOutputFile
            | Self::FileOwner => true,
        }
    }
}

impl std::fmt::Display for ChangedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stdin => "stdin",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::WorkingDirectory => "working directory",
            Self::SignalMask => "signal mask",
            Self::SignalDispositions => "signal dispositions",
            Self::SupplementaryGroups => "supplementary groups",
            Self::Environment => "environment",
            Self::PrimaryGroup => "primary group",
            Self::Pidfile => "pidfile",
            Self::Lockfile => "lockfile",
            Self::OutputFile => "output file",
            Self::FileOwner => "file owner",
            Self::EmptiedOutputFile => "emptied output file",
        })
    }
}

/// What failed: the [`kind`](DaemonizeError::kind) of a [`DaemonizeError`].
///
/// Each variant maps to a `sysexits.h` exit code via [`exit_code`](DaemonizeError::exit_code).
/// `ProgramNotFound` and `ExecFailed` come only from
/// [`DaemonContext::exec`](crate::DaemonContext::exec) and the CLI's checks.
///
/// # Payload stability
///
/// `String` payloads are human-readable display text, not a stable API:
/// match on the variant and [`exit_code`](DaemonizeError::exit_code), not
/// on payload contents. Only named struct fields (e.g.
/// [`LockConflict::path`](ErrorKind::LockConflict),
/// [`Application::code`](ErrorKind::Application)) are stable.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Bad path, bad env key, path overlap, or other config error.
    #[error("validation error: {0}")]
    ValidationError(String),

    /// Program missing or not executable: caught by the CLI's pre-fork path
    /// check, or at exec time by `ENOENT` (missing PATH command or script
    /// interpreter) or `EACCES` (program not executable).
    #[error("program not found: {0}")]
    ProgramNotFound(String),

    /// User does not exist at runtime during the identity switch.
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
    /// the notification channel, opening `/dev/null` or `dup2` onto it,
    /// `sigaction` (resetting signal dispositions), `sigprocmask` (clearing the
    /// signal mask), `getrlimit` (querying the fd limit before closing
    /// inherited descriptors), or reading the thread count around a
    /// `daemonize_with` init.
    #[error("system error: {0}")]
    SystemError(String),

    /// Non-root caller with an identity switch, or an `initgroups`, `setgid`, or
    /// `setuid` failure.
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

    /// Exec of the target program failed for a reason other than
    /// `ENOENT`/`EACCES` (which map to [`ProgramNotFound`](Self::ProgramNotFound)),
    /// or the steps before it failed.
    #[error("exec failed: {0}")]
    ExecFailed(String),

    /// Writing the readiness message to the notification channel failed.
    ///
    /// Produced by [`notify_parent`](crate::DaemonContext::notify_parent) when
    /// the channel write returns an `io::Error`, and by
    /// [`exec`](crate::DaemonContext::exec) when writing the exec marker fails
    /// for a reason other than a launcher that is gone.
    #[error("failed to notify parent: {0}")]
    NotifyFailed(std::io::Error),

    /// The [`daemonize_with`](crate::daemonize_with) init left threads
    /// running before an identity switch to a user, whose `setenv` is not
    /// thread-safe. Holds the thread count. The checked entry point reports it
    /// instead of switching (R126).
    #[error(
        "init left {0} threads running (expected 1); join them before init \
         returns, since switching user calls setenv"
    )]
    InitLeftThreads(usize),

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

impl ErrorKind {
    /// An [`Application`](Self::Application) error; see
    /// [`DaemonizeError::application`].
    pub(crate) fn application(code: u8, message: impl Into<String>) -> Self {
        ErrorKind::Application {
            code,
            message: message.into(),
        }
    }

    /// Returns the `sysexits.h` exit code for this kind; see
    /// [`DaemonizeError::exit_code`].
    pub fn exit_code(&self) -> u8 {
        match self {
            ErrorKind::ValidationError(_) => 64,  // EX_USAGE
            ErrorKind::ProgramNotFound(_) => 66,  // EX_NOINPUT
            ErrorKind::UserNotFound(_) => 67,     // EX_NOUSER
            ErrorKind::GroupNotFound(_) => 67,    // EX_NOUSER
            ErrorKind::LockConflict { .. } => 69, // EX_UNAVAILABLE
            ErrorKind::LockfileError(_) => 73,    // EX_CANTCREAT
            ErrorKind::ForkFailed(_) => 71,       // EX_OSERR
            ErrorKind::SetsidFailed(_) => 71,     // EX_OSERR
            ErrorKind::ChdirFailed(_) => 71,      // EX_OSERR
            ErrorKind::SystemError(_) => 71,      // EX_OSERR
            ErrorKind::PermissionDenied(_) => 77, // EX_NOPERM
            ErrorKind::PidfileError(_) => 73,     // EX_CANTCREAT
            ErrorKind::OutputFileError(_) => 73,  // EX_CANTCREAT
            ErrorKind::ChownError(_) => 73,       // EX_CANTCREAT
            ErrorKind::ExecFailed(_) => 71,       // EX_OSERR
            ErrorKind::NotifyFailed(_) => 71,     // EX_OSERR
            ErrorKind::InitLeftThreads(_) => 70,  // EX_SOFTWARE
            // Caller-chosen, but never 0: that would alias success.
            ErrorKind::Application { code: 0, .. } => 70, // EX_SOFTWARE
            ErrorKind::Application { code, .. } => *code,
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
    /// Maps the error to [`Application`](ErrorKind::Application) with `code` and the
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

    // Covers: R49
    #[test]
    fn exit_codes() {
        assert_eq!(ErrorKind::ValidationError(String::new()).exit_code(), 64);
        assert_eq!(ErrorKind::ProgramNotFound(String::new()).exit_code(), 66);
        assert_eq!(ErrorKind::UserNotFound(String::new()).exit_code(), 67);
        assert_eq!(ErrorKind::GroupNotFound(String::new()).exit_code(), 67);
        assert_eq!(
            ErrorKind::LockConflict {
                path: std::path::PathBuf::new()
            }
            .exit_code(),
            69
        );
        assert_eq!(ErrorKind::LockfileError(String::new()).exit_code(), 73);
        assert_eq!(ErrorKind::ForkFailed(String::new()).exit_code(), 71);
        assert_eq!(ErrorKind::SetsidFailed(String::new()).exit_code(), 71);
        assert_eq!(ErrorKind::ChdirFailed(String::new()).exit_code(), 71);
        assert_eq!(ErrorKind::PermissionDenied(String::new()).exit_code(), 77);
        assert_eq!(ErrorKind::PidfileError(String::new()).exit_code(), 73);
        assert_eq!(ErrorKind::OutputFileError(String::new()).exit_code(), 73);
        assert_eq!(ErrorKind::ChownError(String::new()).exit_code(), 73);
        assert_eq!(ErrorKind::ExecFailed(String::new()).exit_code(), 71);
        assert_eq!(
            ErrorKind::NotifyFailed(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
                .exit_code(),
            71
        );
        assert_eq!(ErrorKind::InitLeftThreads(2).exit_code(), 70);
        // Application errors carry a caller-chosen sysexits code.
        assert_eq!(
            ErrorKind::application(75, "queued").exit_code(),
            75 // EX_TEMPFAIL, chosen by the caller
        );
        // exit_code() is always non-zero: a 0 would make process::exit treat a
        // reported error as success. It is remapped to EX_SOFTWARE.
        assert_eq!(ErrorKind::application(0, "boom").exit_code(), 70);
        assert_eq!(
            ErrorKind::application(71, "bind failed").to_string(),
            "application error: bind failed"
        );
    }

    /// One sample of every variant, by hand. [`variant_name`] is the one
    /// exhaustive match over `ErrorKind`: a new variant fails to compile
    /// there, which is the prompt to sample it here. Nothing checks that it
    /// was: a variant left out of this list escapes the SPEC guard below.
    /// Every test that needs every variant iterates this list.
    fn every_variant() -> Vec<ErrorKind> {
        vec![
            ErrorKind::ValidationError("detail".into()),
            ErrorKind::ProgramNotFound("detail".into()),
            ErrorKind::UserNotFound("detail".into()),
            ErrorKind::GroupNotFound("detail".into()),
            ErrorKind::LockConflict {
                path: "/detail".into(),
            },
            ErrorKind::LockfileError("detail".into()),
            ErrorKind::ForkFailed("detail".into()),
            ErrorKind::SetsidFailed("detail".into()),
            ErrorKind::ChdirFailed("detail".into()),
            ErrorKind::SystemError("detail".into()),
            ErrorKind::PermissionDenied("detail".into()),
            ErrorKind::PidfileError("detail".into()),
            ErrorKind::OutputFileError("detail".into()),
            ErrorKind::ChownError("detail".into()),
            ErrorKind::ExecFailed("detail".into()),
            ErrorKind::NotifyFailed(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            ErrorKind::InitLeftThreads(2),
            ErrorKind::application(42, "detail"),
        ]
    }

    /// The variant's name, as the SPEC table writes it.
    fn variant_name(err: &ErrorKind) -> &'static str {
        match err {
            ErrorKind::ValidationError(_) => "ValidationError",
            ErrorKind::ProgramNotFound(_) => "ProgramNotFound",
            ErrorKind::UserNotFound(_) => "UserNotFound",
            ErrorKind::GroupNotFound(_) => "GroupNotFound",
            ErrorKind::LockConflict { .. } => "LockConflict",
            ErrorKind::LockfileError(_) => "LockfileError",
            ErrorKind::ForkFailed(_) => "ForkFailed",
            ErrorKind::SetsidFailed(_) => "SetsidFailed",
            ErrorKind::ChdirFailed(_) => "ChdirFailed",
            ErrorKind::SystemError(_) => "SystemError",
            ErrorKind::PermissionDenied(_) => "PermissionDenied",
            ErrorKind::PidfileError(_) => "PidfileError",
            ErrorKind::OutputFileError(_) => "OutputFileError",
            ErrorKind::ChownError(_) => "ChownError",
            ErrorKind::ExecFailed(_) => "ExecFailed",
            ErrorKind::NotifyFailed(_) => "NotifyFailed",
            ErrorKind::InitLeftThreads(_) => "InitLeftThreads",
            ErrorKind::Application { .. } => "Application",
        }
    }

    // Covers: R83
    #[test]
    fn display_messages_are_lowercase_without_trailing_punctuation() {
        for err in every_variant() {
            let msg = err.to_string();
            let first = msg.chars().next().expect("message is non-empty");
            assert!(
                !first.is_ascii_uppercase(),
                "message must start lowercase, got: {msg:?}"
            );
            assert!(
                !msg.ends_with(['.', '!', '?']),
                "message must not end with punctuation, got: {msg:?}"
            );
        }
    }

    /// SPEC, baked in at compile time.
    ///
    /// Never read at run time: this test ships with the crate, and a run-time
    /// read would panic for anyone running `cargo test` on a packaged or
    /// vendored copy. `just package-test` runs it on the packaged copy.
    const SPEC: &str = include_str!("../docs/SPEC.md");

    /// SPEC's exit-code table restates `DaemonizeError`: its variants and
    /// what `exit_code()` returns for each. Nothing outside the crate can
    /// enumerate a `#[non_exhaustive]` enum, so this guard lives here, in the
    /// crate. The table must list every variant, no more, and each code must
    /// match.
    #[test]
    fn spec_exit_codes_match_error_impl() {
        let documented = parse_exit_code_table();
        let names: std::collections::BTreeSet<&str> =
            every_variant().iter().map(variant_name).collect();
        let table: std::collections::BTreeSet<&str> =
            documented.keys().map(String::as_str).collect();
        assert_eq!(
            names, table,
            "SPEC exit-code table and DaemonizeError variants must match exactly"
        );

        for err in every_variant() {
            let name = variant_name(&err);
            let cell = &documented[name];
            if name == "Application" {
                assert!(
                    cell.starts_with("caller's"),
                    "Application row should read \"caller's\", got {cell:?}"
                );
                assert_eq!(
                    err.exit_code(),
                    42,
                    "Application forwards the caller's code"
                );
            } else {
                let code: u8 = cell
                    .parse()
                    .unwrap_or_else(|_| panic!("{name} exit code cell '{cell}' is not a number"));
                assert_eq!(code, err.exit_code(), "{name} exit code drifted from SPEC");
            }
        }
    }

    /// Parse SPEC's "Exit codes" table into `variant -> code cell`.
    fn parse_exit_code_table() -> std::collections::BTreeMap<String, String> {
        let section = SPEC
            .split("### Exit codes")
            .nth(1)
            .expect("SPEC has an 'Exit codes' section")
            .split("\n## ")
            .next()
            .unwrap();

        let mut map = std::collections::BTreeMap::new();
        for line in section.lines() {
            let line = line.trim();
            // Data rows start with a backticked variant name; the header
            // (`ErrorKind` variant) and the `| --- |` separator do not.
            if !line.starts_with("| `") || line.contains("variant") {
                continue;
            }
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            map.insert(cells[1].trim_matches('`').to_string(), cells[2].to_string());
        }
        assert!(!map.is_empty(), "parsed no rows from the exit-code table");
        map
    }

    // Covers: R154
    #[test]
    fn rollback_failures_leave_the_message_alone() {
        let err = DaemonizeError::application(75, "init failed").with_rollback_failures([
            RestoreFailure::new(
                ChangedState::WorkingDirectory,
                std::io::Error::from_raw_os_error(libc::EBADF),
            ),
        ]);
        assert_eq!(err.to_string(), "application error: init failed");
    }

    #[test]
    fn rollback_failures_keep_their_order() {
        let failure = |state| RestoreFailure::new(state, std::io::ErrorKind::Other);
        let err = DaemonizeError::from(ErrorKind::SystemError("x".into()))
            .with_rollback_failures([failure(ChangedState::Stdout)])
            .with_rollback_failures([failure(ChangedState::Pidfile)]);
        let states: Vec<_> = err
            .rollback_failures()
            .iter()
            .map(RestoreFailure::state)
            .collect();
        assert_eq!(states, [ChangedState::Stdout, ChangedState::Pidfile]);
    }

    // The message already names the io::Error; a source would print it
    // twice in an error chain.
    #[test]
    fn a_failed_notify_names_its_cause_once() {
        use std::error::Error;
        let err = DaemonizeError::from(ErrorKind::NotifyFailed(
            std::io::ErrorKind::BrokenPipe.into(),
        ));
        assert!(err.source().is_none());
    }

    #[test]
    fn a_restore_failure_names_the_state_and_why() {
        let failure = RestoreFailure::new(
            ChangedState::SupplementaryGroups,
            std::io::Error::from_raw_os_error(libc::EPERM),
        );
        assert_eq!(
            failure.to_string(),
            format!(
                "supplementary groups: {}",
                std::io::Error::from_raw_os_error(libc::EPERM)
            )
        );
    }

    #[test]
    fn a_restore_failure_on_a_file_names_its_path() {
        let failure = RestoreFailure::at(
            ChangedState::Pidfile,
            std::path::Path::new("/run/app.pid"),
            std::io::Error::from_raw_os_error(libc::EACCES),
        );
        assert_eq!(
            failure.to_string(),
            format!(
                "pidfile /run/app.pid: {}",
                std::io::Error::from_raw_os_error(libc::EACCES)
            )
        );
    }

    #[test]
    fn only_file_changes_outlive_the_process() {
        use ChangedState::*;
        let lasting: Vec<_> = ALL_STATES
            .into_iter()
            .filter(|s| s.outlives_process())
            .collect();
        assert_eq!(
            lasting,
            [Pidfile, Lockfile, OutputFile, FileOwner, EmptiedOutputFile]
        );
    }

    #[test]
    fn a_restore_failure_names_its_cause_once() {
        use std::error::Error;
        let failure = RestoreFailure::new(ChangedState::Stdin, std::io::ErrorKind::BrokenPipe);
        assert!(failure.source().is_none());
    }

    // Covers: R83
    #[test]
    fn changed_state_names_are_lowercase() {
        for state in ALL_STATES {
            let name = state.to_string();
            assert!(!name.is_empty() && name == name.to_lowercase(), "{name:?}");
        }
    }

    /// Every [`ChangedState`], in declaration order.
    const ALL_STATES: [ChangedState; 14] = {
        use ChangedState::*;
        [
            Stdin,
            Stdout,
            Stderr,
            WorkingDirectory,
            SignalMask,
            SignalDispositions,
            SupplementaryGroups,
            PrimaryGroup,
            Pidfile,
            Lockfile,
            OutputFile,
            FileOwner,
            Environment,
            EmptiedOutputFile,
        ]
    };

    /// A new variant fails to compile here: the prompt to give it the next
    /// position and add it to [`ALL_STATES`] in that place.
    fn position(state: ChangedState) -> usize {
        use ChangedState::*;
        match state {
            Stdin => 0,
            Stdout => 1,
            Stderr => 2,
            WorkingDirectory => 3,
            SignalMask => 4,
            SignalDispositions => 5,
            SupplementaryGroups => 6,
            PrimaryGroup => 7,
            Pidfile => 8,
            Lockfile => 9,
            OutputFile => 10,
            FileOwner => 11,
            Environment => 12,
            EmptiedOutputFile => 13,
        }
    }

    #[test]
    fn all_states_lists_every_variant_once() {
        let positions: Vec<_> = ALL_STATES.into_iter().map(position).collect();
        assert_eq!(positions, (0..ALL_STATES.len()).collect::<Vec<_>>());
    }

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
