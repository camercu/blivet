//! Daemon configuration with builder pattern and pre-fork validation.

use std::path::PathBuf;

use nix::sys::stat::Mode;

use crate::error::DaemonizeError;
use crate::util::paths_same;

/// Configuration for the daemonization process.
///
/// All fields are private; use builder methods to configure.
/// All builder methods are infallible; validation is centralized in [`validate`](DaemonConfig::validate).
///
/// # Example
///
/// ```
/// use blivet::DaemonConfig;
///
/// let mut config = DaemonConfig::new();
/// config.pidfile("/var/run/foo.pid").chdir("/tmp");
/// ```
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DaemonConfig {
    pub(crate) pidfile: Option<PathBuf>,
    pub(crate) chdir: PathBuf,
    pub(crate) umask: Mode,
    pub(crate) stdout: Option<PathBuf>,
    pub(crate) stderr: Option<PathBuf>,
    pub(crate) append: bool,
    pub(crate) lockfile: Option<PathBuf>,
    pub(crate) user: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) foreground: bool,
    pub(crate) close_fds: bool,
    pub(crate) env: Vec<(String, String)>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            pidfile: None,
            chdir: PathBuf::from("/"),
            umask: Mode::empty(),
            stdout: None,
            stderr: None,
            append: false,
            lockfile: None,
            user: None,
            group: None,
            foreground: false,
            close_fds: true,
            env: Vec::new(),
        }
    }
}

impl DaemonConfig {
    /// Creates a new `DaemonConfig` with default values.
    ///
    /// Equivalent to [`Default::default()`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the pidfile path. Default: none.
    pub fn pidfile(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.pidfile = Some(path.into());
        self
    }

    /// Sets the working directory. Default: `/`.
    pub fn chdir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.chdir = path.into();
        self
    }

    /// Sets the process umask. Default: `Mode::empty()` (0).
    pub fn umask(&mut self, mode: Mode) -> &mut Self {
        self.umask = mode;
        self
    }

    /// Sets the stdout redirect file path. Default: none (stays `/dev/null`).
    pub fn stdout(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.stdout = Some(path.into());
        self
    }

    /// Sets the stderr redirect file path. Default: none (stays `/dev/null`).
    pub fn stderr(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.stderr = Some(path.into());
        self
    }

    /// Sets whether to append to stdout/stderr files. Default: `false`.
    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    /// Sets the lockfile path. Default: none.
    pub fn lockfile(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.lockfile = Some(path.into());
        self
    }

    /// Sets the user to run the daemon as. Default: none (no user switch).
    ///
    /// Accepts a username string or a numeric UID (as a string, e.g. `"1000"`).
    /// Resolution happens at runtime in [`DaemonContext::drop_privileges`](crate::DaemonContext::drop_privileges).
    pub fn user(&mut self, name: impl Into<String>) -> &mut Self {
        self.user = Some(name.into());
        self
    }

    /// Sets the group to run the daemon as. Default: none (use user's primary group).
    ///
    /// Accepts a group name string or a numeric GID (as a string, e.g. `"1000"`).
    /// Resolution happens at runtime in [`DaemonContext::drop_privileges`](crate::DaemonContext::drop_privileges).
    pub fn group(&mut self, name: impl Into<String>) -> &mut Self {
        self.group = Some(name.into());
        self
    }

    /// Sets foreground mode. Default: `false`.
    ///
    /// When `true`, daemonization skips both forks, `setsid`, and the
    /// notification pipe. All other steps (umask, chdir, redirect, signal
    /// reset, etc.) still execute.
    pub fn foreground(&mut self, foreground: bool) -> &mut Self {
        self.foreground = foreground;
        self
    }

    /// Sets whether to close inherited file descriptors. Default: `true`.
    ///
    /// When `false`, file descriptors 3+ are left open. Useful in
    /// foreground mode when running under a supervisor that passes
    /// file descriptors.
    pub fn close_fds(&mut self, close_fds: bool) -> &mut Self {
        self.close_fds = close_fds;
        self
    }

    /// Adds an environment variable. Each call accumulates; last-write-wins
    /// for duplicate keys at application time.
    pub fn env(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Validates the configuration.
    ///
    /// Performs minimal I/O: checks path existence, directory writability
    /// (via `faccessat(AT_EACCESS)`), and queries the effective UID when a
    /// user switch is configured. No files are created or modified.
    ///
    /// # Errors
    ///
    /// Returns `DaemonizeError::ValidationError` if:
    /// - Any configured path (pidfile, stdout, stderr, lockfile) is not absolute
    /// - The chdir path is not absolute, does not exist, or is not a directory
    /// - The pidfile path is a directory
    /// - Parent directories of configured paths are not writable
    /// - Lockfile or pidfile overlaps with stdout or stderr
    /// - An environment key is empty or contains `=`
    /// - A user is configured but the effective UID is not 0
    #[must_use = "validate() returns a Result that must be checked"]
    pub fn validate(&self) -> Result<(), DaemonizeError> {
        // Check chdir is absolute, exists, and is a directory
        if !self.chdir.is_absolute() {
            return Err(DaemonizeError::ValidationError(
                "chdir path must be absolute".into(),
            ));
        }
        if !self.chdir.exists() {
            return Err(DaemonizeError::ValidationError(
                "chdir path does not exist".into(),
            ));
        }
        if !self.chdir.is_dir() {
            return Err(DaemonizeError::ValidationError(
                "chdir path is not a directory".into(),
            ));
        }

        // Check pidfile
        if let Some(ref p) = self.pidfile {
            validate_absolute(p, "pidfile")?;
            if p.is_dir() {
                return Err(DaemonizeError::ValidationError(
                    "pidfile path is a directory".into(),
                ));
            }
            validate_parent_writable(p, "pidfile")?;
        }

        // Check stdout
        if let Some(ref p) = self.stdout {
            validate_absolute(p, "stdout")?;
            validate_parent_writable(p, "stdout")?;
        }

        // Check stderr
        if let Some(ref p) = self.stderr {
            validate_absolute(p, "stderr")?;
            validate_parent_writable(p, "stderr")?;
        }

        // Check lockfile
        if let Some(ref p) = self.lockfile {
            validate_absolute(p, "lockfile")?;
            validate_parent_writable(p, "lockfile")?;
        }

        // Path overlap checks: lockfile/pidfile must not equal stdout/stderr
        if let Some(ref lockfile) = self.lockfile {
            if let Some(ref stdout) = self.stdout {
                if paths_same(lockfile, stdout) {
                    return Err(DaemonizeError::ValidationError(
                        "lockfile and stdout must not be the same path".into(),
                    ));
                }
            }
            if let Some(ref stderr) = self.stderr {
                if paths_same(lockfile, stderr) {
                    return Err(DaemonizeError::ValidationError(
                        "lockfile and stderr must not be the same path".into(),
                    ));
                }
            }
        }
        if let Some(ref pidfile) = self.pidfile {
            if let Some(ref stdout) = self.stdout {
                if paths_same(pidfile, stdout) {
                    return Err(DaemonizeError::ValidationError(
                        "pidfile and stdout must not be the same path".into(),
                    ));
                }
            }
            if let Some(ref stderr) = self.stderr {
                if paths_same(pidfile, stderr) {
                    return Err(DaemonizeError::ValidationError(
                        "pidfile and stderr must not be the same path".into(),
                    ));
                }
            }
        }

        // Environment key validation
        for (key, _) in &self.env {
            if key.is_empty() {
                return Err(DaemonizeError::ValidationError(
                    "environment key must not be empty".into(),
                ));
            }
            if key.contains('=') {
                return Err(DaemonizeError::ValidationError(format!(
                    "environment key must not contain '=': {key}"
                )));
            }
        }

        // User/group validation: must be root to switch users or groups
        if (self.user.is_some() || self.group.is_some()) && nix::unistd::geteuid().as_raw() != 0 {
            return Err(DaemonizeError::PermissionDenied(
                "must be root to switch users or groups".into(),
            ));
        }

        Ok(())
    }
}

fn validate_absolute(path: &std::path::Path, name: &str) -> Result<(), DaemonizeError> {
    if !path.is_absolute() {
        return Err(DaemonizeError::ValidationError(format!(
            "{name} path must be absolute"
        )));
    }
    Ok(())
}

fn validate_parent_writable(path: &std::path::Path, name: &str) -> Result<(), DaemonizeError> {
    use nix::fcntl::AtFlags;
    use nix::unistd::AccessFlags;

    let parent = path.parent().ok_or_else(|| {
        DaemonizeError::ValidationError(format!("{name} path has no parent directory"))
    })?;
    if !parent.exists() {
        return Err(DaemonizeError::ValidationError(format!(
            "{name} parent directory does not exist"
        )));
    }
    // Check writability using faccessat(AT_EACCESS) which tests against the
    // effective UID/GID rather than the real UID (important for setuid binaries).
    match nix::unistd::faccessat(
        crate::unsafe_ops::at_fdcwd(),
        parent,
        AccessFlags::W_OK,
        AtFlags::AT_EACCESS,
    ) {
        Ok(()) => Ok(()),
        Err(_) => Err(DaemonizeError::ValidationError(format!(
            "{name} parent directory is not writable"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_equals_default() {
        assert_eq!(DaemonConfig::new(), DaemonConfig::default());
    }

    #[test]
    fn default_values() {
        let config = DaemonConfig::default();
        assert_eq!(config.pidfile, None);
        assert_eq!(config.chdir, PathBuf::from("/"));
        assert_eq!(config.umask, Mode::empty());
        assert_eq!(config.stdout, None);
        assert_eq!(config.stderr, None);
        assert!(!config.append);
        assert_eq!(config.lockfile, None);
        assert_eq!(config.user, None);
        assert_eq!(config.group, None);
        assert!(!config.foreground);
        assert!(config.close_fds);
        assert!(config.env.is_empty());
    }

    #[test]
    fn builder_setters_replace() {
        let mut config = DaemonConfig::new();
        config.pidfile("/a").pidfile("/b");
        assert_eq!(config.pidfile, Some(PathBuf::from("/b")));
    }

    #[test]
    fn env_accumulates() {
        let mut config = DaemonConfig::new();
        config.env("A", "1").env("B", "2").env("A", "3");
        assert_eq!(
            config.env,
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "2".into()),
                ("A".into(), "3".into()),
            ]
        );
    }

    #[test]
    fn validate_chdir_must_be_absolute() {
        let mut config = DaemonConfig::new();
        config.chdir("relative/path");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_chdir_must_exist() {
        let mut config = DaemonConfig::new();
        config.chdir("/nonexistent_daemonize_test_dir");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_pidfile_must_be_absolute() {
        let mut config = DaemonConfig::new();
        config.pidfile("relative.pid");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_pidfile_not_directory() {
        let mut config = DaemonConfig::new();
        config.pidfile("/tmp");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_stdout_must_be_absolute() {
        let mut config = DaemonConfig::new();
        config.stdout("relative.log");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_stderr_must_be_absolute() {
        let mut config = DaemonConfig::new();
        config.stderr("relative.log");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_lockfile_must_be_absolute() {
        let mut config = DaemonConfig::new();
        config.lockfile("relative.lock");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_lockfile_pidfile_same_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("combined.pid");
        let path_str = path.to_str().unwrap();
        let mut config = DaemonConfig::new();
        config.lockfile(path_str).pidfile(path_str);
        // Should not fail on overlap between lockfile and pidfile
        // (may fail for other reasons like non-root user, but not overlap)
        let result = config.validate();
        assert!(
            !matches!(&result, Err(DaemonizeError::ValidationError(msg)) if msg.contains("same path"))
        );
    }

    #[test]
    fn validate_lockfile_stdout_overlap_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.log");
        let path_str = path.to_str().unwrap();
        let mut config = DaemonConfig::new();
        config.lockfile(path_str).stdout(path_str);
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_pidfile_stderr_overlap_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.log");
        let path_str = path.to_str().unwrap();
        let mut config = DaemonConfig::new();
        config.pidfile(path_str).stderr(path_str);
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_env_key_empty_rejected() {
        let mut config = DaemonConfig::new();
        config.env("", "value");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_env_key_with_equals_rejected() {
        let mut config = DaemonConfig::new();
        config.env("KEY=BAD", "value");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_default_config_ok() {
        // Default config should validate (we're not root, no user switch)
        assert!(DaemonConfig::new().validate().is_ok());
    }

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
        assert_eq!(DaemonizeError::LockConflict(String::new()).exit_code(), 69);
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
    }

    #[test]
    fn send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DaemonConfig>();
        assert_send_sync::<crate::DaemonContext>();
        assert_send_sync::<DaemonizeError>();
    }

    #[test]
    fn validate_chdir_must_be_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not_a_dir");
        std::fs::write(&file, "").unwrap();
        let mut config = DaemonConfig::new();
        config.chdir(&file);
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(msg)) if msg.contains("not a directory")
        ));
    }

    #[test]
    fn validate_lockfile_stderr_overlap_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.log");
        let path_str = path.to_str().unwrap();
        let mut config = DaemonConfig::new();
        config.lockfile(path_str).stderr(path_str);
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_pidfile_stdout_overlap_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.log");
        let path_str = path.to_str().unwrap();
        let mut config = DaemonConfig::new();
        config.pidfile(path_str).stdout(path_str);
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    #[test]
    fn validate_pidfile_parent_nonwritable() {
        let mut config = DaemonConfig::new();
        config.pidfile("/nonexistent_parent_dir_xyz/test.pid");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(msg)) if msg.contains("parent")
        ));
    }

    #[test]
    fn validate_stdout_parent_nonwritable() {
        let mut config = DaemonConfig::new();
        config.stdout("/nonexistent_parent_dir_xyz/test.log");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(msg)) if msg.contains("parent")
        ));
    }

    #[test]
    fn validate_stderr_parent_nonwritable() {
        let mut config = DaemonConfig::new();
        config.stderr("/nonexistent_parent_dir_xyz/test.log");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(msg)) if msg.contains("parent")
        ));
    }

    #[test]
    fn paths_same_canonicalize_fallback() {
        // Paths that don't exist — canonicalize will fail, should fall back to byte comparison
        assert!(paths_same(
            std::path::Path::new("/nonexistent/a"),
            std::path::Path::new("/nonexistent/a"),
        ));
        assert!(!paths_same(
            std::path::Path::new("/nonexistent/a"),
            std::path::Path::new("/nonexistent/b"),
        ));
    }

    #[test]
    fn display_includes_prefix() {
        let err = DaemonizeError::ValidationError("test message".into());
        assert_eq!(err.to_string(), "validation error: test message");
    }

    #[test]
    fn validate_rejects_invalid_config_before_fork() {
        // Verify validate() catches errors that would otherwise only surface post-fork
        let mut config = DaemonConfig::new();
        config.pidfile("relative.pid");
        let result = config.validate();
        assert!(result.is_err());
        // The important thing: this was checked without forking
    }

    #[test]
    fn group_builder_sets_field() {
        let mut config = DaemonConfig::new();
        config.group("wheel");
        assert_eq!(config.group, Some("wheel".into()));
    }

    #[test]
    fn foreground_builder_sets_field() {
        let mut config = DaemonConfig::new();
        config.foreground(true);
        assert!(config.foreground);
    }

    #[test]
    fn close_fds_builder_sets_field() {
        let mut config = DaemonConfig::new();
        config.close_fds(false);
        assert!(!config.close_fds);
    }

    #[test]
    fn validate_group_requires_root() {
        // Non-root with group should fail validation
        if nix::unistd::geteuid().as_raw() != 0 {
            let mut config = DaemonConfig::new();
            config.group("wheel");
            assert!(matches!(
                config.validate(),
                Err(DaemonizeError::PermissionDenied(_))
            ));
        }
    }

    #[test]
    fn validate_user_or_group_requires_root() {
        // Non-root with user should fail validation (existing behavior)
        if nix::unistd::geteuid().as_raw() != 0 {
            let mut config = DaemonConfig::new();
            config.user("nobody");
            assert!(matches!(
                config.validate(),
                Err(DaemonizeError::PermissionDenied(_))
            ));
        }
    }
}
