//! Error types for the daemonization sequence, with `sysexits.h` exit codes.

/// Errors produced during configuration validation or the daemonization sequence.
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
/// [`LockConflict::path`](DaemonizeError::LockConflict),
/// [`Application::code`](DaemonizeError::Application)) are stable.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DaemonizeError {
    /// Bad path, bad env key, path overlap, or other config error.
    #[error("validation error: {0}")]
    ValidationError(String),

    /// Program missing or not executable: caught by the CLI's pre-fork path
    /// check, or at exec time by `ENOENT` (missing PATH command or script
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

    /// Exec of the target program failed for a reason other than
    /// `ENOENT`/`EACCES` (which map to [`ProgramNotFound`](Self::ProgramNotFound)),
    /// or the steps before it failed.
    #[error("exec failed: {0}")]
    ExecFailed(String),

    /// Writing the readiness message to the parent notification pipe failed.
    ///
    /// Produced by [`notify_parent`](crate::DaemonContext::notify_parent) when
    /// the pipe write returns an `io::Error`.
    #[error("failed to notify parent: {0}")]
    NotifyFailed(#[source] std::io::Error),

    /// The [`daemonize_with`](crate::daemonize_with) init left threads
    /// running before a user switch, whose `setenv` is not thread-safe. Holds
    /// the thread count. The checked entry point reports it instead of
    /// switching (R126).
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
            DaemonizeError::InitLeftThreads(_) => 70,  // EX_SOFTWARE
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

    // Covers: R49
    #[test]
    fn exit_codes() {
        assert_eq!(
            DaemonizeError::ValidationError(String::new()).exit_code(),
            64
        );
        assert_eq!(
            DaemonizeError::ProgramNotFound(String::new()).exit_code(),
            66
        );
        assert_eq!(DaemonizeError::UserNotFound(String::new()).exit_code(), 67);
        assert_eq!(DaemonizeError::GroupNotFound(String::new()).exit_code(), 67);
        assert_eq!(
            DaemonizeError::LockConflict {
                path: std::path::PathBuf::new()
            }
            .exit_code(),
            69
        );
        assert_eq!(DaemonizeError::LockfileError(String::new()).exit_code(), 73);
        assert_eq!(DaemonizeError::ForkFailed(String::new()).exit_code(), 71);
        assert_eq!(DaemonizeError::SetsidFailed(String::new()).exit_code(), 71);
        assert_eq!(DaemonizeError::ChdirFailed(String::new()).exit_code(), 71);
        assert_eq!(
            DaemonizeError::PermissionDenied(String::new()).exit_code(),
            77
        );
        assert_eq!(DaemonizeError::PidfileError(String::new()).exit_code(), 73);
        assert_eq!(
            DaemonizeError::OutputFileError(String::new()).exit_code(),
            73
        );
        assert_eq!(DaemonizeError::ChownError(String::new()).exit_code(), 73);
        assert_eq!(DaemonizeError::ExecFailed(String::new()).exit_code(), 71);
        assert_eq!(
            DaemonizeError::NotifyFailed(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
                .exit_code(),
            71
        );
        assert_eq!(DaemonizeError::InitLeftThreads(2).exit_code(), 70);
        // Application errors carry a caller-chosen sysexits code.
        assert_eq!(
            DaemonizeError::application(75, "queued").exit_code(),
            75 // EX_TEMPFAIL, chosen by the caller
        );
        // exit_code() is always non-zero: a 0 would make process::exit treat a
        // reported error as success. It is remapped to EX_SOFTWARE.
        assert_eq!(DaemonizeError::application(0, "boom").exit_code(), 70);
        assert_eq!(
            DaemonizeError::application(71, "bind failed").to_string(),
            "application error: bind failed"
        );
    }

    /// One sample of every variant. [`variant_name`] is the one exhaustive
    /// match over `DaemonizeError`: a new variant fails to compile there, and
    /// the README guard below fails until the variant is sampled here and
    /// listed in the README table. Every test that needs every variant
    /// iterates this list.
    fn every_variant() -> Vec<DaemonizeError> {
        vec![
            DaemonizeError::ValidationError("detail".into()),
            DaemonizeError::ProgramNotFound("detail".into()),
            DaemonizeError::UserNotFound("detail".into()),
            DaemonizeError::GroupNotFound("detail".into()),
            DaemonizeError::LockConflict {
                path: "/detail".into(),
            },
            DaemonizeError::LockfileError("detail".into()),
            DaemonizeError::ForkFailed("detail".into()),
            DaemonizeError::SetsidFailed("detail".into()),
            DaemonizeError::ChdirFailed("detail".into()),
            DaemonizeError::SystemError("detail".into()),
            DaemonizeError::PermissionDenied("detail".into()),
            DaemonizeError::PidfileError("detail".into()),
            DaemonizeError::OutputFileError("detail".into()),
            DaemonizeError::ChownError("detail".into()),
            DaemonizeError::ExecFailed("detail".into()),
            DaemonizeError::NotifyFailed(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            DaemonizeError::InitLeftThreads(2),
            DaemonizeError::application(42, "detail"),
        ]
    }

    /// The variant's name, as the README table writes it.
    fn variant_name(err: &DaemonizeError) -> &'static str {
        match err {
            DaemonizeError::ValidationError(_) => "ValidationError",
            DaemonizeError::ProgramNotFound(_) => "ProgramNotFound",
            DaemonizeError::UserNotFound(_) => "UserNotFound",
            DaemonizeError::GroupNotFound(_) => "GroupNotFound",
            DaemonizeError::LockConflict { .. } => "LockConflict",
            DaemonizeError::LockfileError(_) => "LockfileError",
            DaemonizeError::ForkFailed(_) => "ForkFailed",
            DaemonizeError::SetsidFailed(_) => "SetsidFailed",
            DaemonizeError::ChdirFailed(_) => "ChdirFailed",
            DaemonizeError::SystemError(_) => "SystemError",
            DaemonizeError::PermissionDenied(_) => "PermissionDenied",
            DaemonizeError::PidfileError(_) => "PidfileError",
            DaemonizeError::OutputFileError(_) => "OutputFileError",
            DaemonizeError::ChownError(_) => "ChownError",
            DaemonizeError::ExecFailed(_) => "ExecFailed",
            DaemonizeError::NotifyFailed(_) => "NotifyFailed",
            DaemonizeError::InitLeftThreads(_) => "InitLeftThreads",
            DaemonizeError::Application { .. } => "Application",
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

    /// The README, baked in at compile time.
    ///
    /// Never read at run time: this test ships with the crate, and a run-time
    /// read would panic for anyone running `cargo test` on a packaged or
    /// vendored copy. `just package-test` runs it on the packaged copy.
    const README: &str = include_str!("../README.md");

    /// The README's exit-code table restates `DaemonizeError`: its variants
    /// and what `exit_code()` returns for each. It cannot be generated (the
    /// "Meaning" column is prose) and nothing outside the crate can enumerate
    /// a `#[non_exhaustive]` enum, so this guard lives here, in the crate.
    /// The table must list every variant, no more, and each code must match.
    #[test]
    fn readme_exit_codes_match_error_impl() {
        let documented = parse_exit_code_table();
        let names: std::collections::BTreeSet<&str> =
            every_variant().iter().map(variant_name).collect();
        let table: std::collections::BTreeSet<&str> =
            documented.keys().map(String::as_str).collect();
        assert_eq!(
            names, table,
            "README exit-code table and DaemonizeError variants must match exactly"
        );

        for err in every_variant() {
            let name = variant_name(&err);
            let cell = &documented[name];
            if name == "Application" {
                assert_eq!(cell, "caller's", "Application row should read \"caller's\"");
                assert_eq!(
                    err.exit_code(),
                    42,
                    "Application forwards the caller's code"
                );
            } else {
                let code: u8 = cell
                    .parse()
                    .unwrap_or_else(|_| panic!("{name} exit code cell '{cell}' is not a number"));
                assert_eq!(
                    code,
                    err.exit_code(),
                    "{name} exit code drifted from the README"
                );
            }
        }
    }

    /// Parse the README "Errors & exit codes" table into `variant -> code cell`.
    fn parse_exit_code_table() -> std::collections::BTreeMap<String, String> {
        let section = README
            .split("### Errors & exit codes")
            .nth(1)
            .expect("README has an 'Errors & exit codes' section")
            .split("\n## ")
            .next()
            .unwrap();

        let mut map = std::collections::BTreeMap::new();
        for line in section.lines() {
            let line = line.trim();
            // Data rows start with a backticked variant name; the header and
            // the `| --- |` separator do not.
            if !line.starts_with("| `") {
                continue;
            }
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            map.insert(cells[1].trim_matches('`').to_string(), cells[2].to_string());
        }
        assert!(!map.is_empty(), "parsed no rows from the exit-code table");
        map
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
