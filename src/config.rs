//! Daemon configuration with builder pattern and pre-fork validation.

use std::path::PathBuf;

use crate::error::DaemonizeError;
use crate::util::paths_same;

/// How the lockfile is determined. Tri-state so "not set" (derive from the
/// pidfile) is distinguishable from an explicit opt-out.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) enum LockfileSetting {
    /// Default: lock the pidfile itself when one is configured.
    DeriveFromPidfile,
    /// Explicit path from [`DaemonConfig::lockfile`].
    Path(PathBuf),
    /// Explicit opt-out from [`DaemonConfig::no_lockfile`].
    Disabled,
}

/// Configuration for the daemonization process.
///
/// All fields are private; use builder methods to configure.
/// All builder methods are infallible; validation is centralized in [`validate`](DaemonConfig::validate).
///
/// This is a **non-consuming** builder: setters take `&mut self` and return
/// `&mut Self`, so you mutate a binding in place rather than chaining off
/// [`new`](DaemonConfig::new). Because of that, `DaemonConfig::new().pidfile(..)`
/// evaluates to `&mut DaemonConfig`, not an owned value — to build a config in a
/// helper, mutate a local and return it by value (or `.clone()` a shared one):
///
/// ```
/// use blivet::DaemonConfig;
///
/// fn make_config(pid: &str) -> DaemonConfig {
///     let mut config = DaemonConfig::new();
///     config.pidfile(pid).chdir("/var/lib/foo");
///     config
/// }
/// ```
///
/// # Example
///
/// ```
/// use blivet::DaemonConfig;
///
/// let mut config = DaemonConfig::new();
/// config.pidfile("/var/run/foo.pid").chdir("/var/lib/foo");
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct DaemonConfig {
    pub(crate) pidfile: Option<PathBuf>,
    pub(crate) chdir: PathBuf,
    /// Process umask as an octal permission value (`<= 0o7777`). Range is
    /// enforced by [`validate`](DaemonConfig::validate).
    pub(crate) umask: u32,
    pub(crate) stdout: Option<PathBuf>,
    pub(crate) stderr: Option<PathBuf>,
    pub(crate) append: bool,
    pub(crate) lockfile: LockfileSetting,
    pub(crate) user: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) foreground: bool,
    pub(crate) close_fds: bool,
    pub(crate) cleanup_on_drop: bool,
    pub(crate) chown_paths: bool,
    pub(crate) env: Vec<(String, String)>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            pidfile: None,
            chdir: PathBuf::from("/"),
            umask: 0,
            stdout: None,
            stderr: None,
            append: false,
            lockfile: LockfileSetting::DeriveFromPidfile,
            user: None,
            group: None,
            foreground: false,
            close_fds: true,
            cleanup_on_drop: true,
            chown_paths: true,
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
    ///
    /// Unless a separate [`lockfile`](DaemonConfig::lockfile) is set, the
    /// pidfile is also exclusively flock'd, so a second instance fails with
    /// [`LockConflict`](crate::DaemonizeError::LockConflict) instead of
    /// silently overwriting the pidfile. Opt out with
    /// [`no_lockfile`](DaemonConfig::no_lockfile).
    pub fn pidfile(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.pidfile = Some(path.into());
        self
    }

    /// Sets the working directory. Default: `/`.
    ///
    /// Because the default is `/`, any **relative** path your daemon uses
    /// afterward (log files, sockets, config) resolves against `/` and will
    /// usually fail. Use absolute paths, or set this to your working directory.
    pub fn chdir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.chdir = path.into();
        self
    }

    /// Sets the process umask as an octal permission value, e.g. `0o022`.
    /// Default: `0`.
    ///
    /// Takes a plain integer so callers need no third-party type (and no
    /// matching `nix` version) just to set a umask. The value must fit in the
    /// 12 permission bits (`<= 0o7777`); larger values are rejected by
    /// [`validate`](DaemonConfig::validate) with a
    /// [`ValidationError`](crate::DaemonizeError::ValidationError).
    ///
    /// ```
    /// use blivet::DaemonConfig;
    ///
    /// let mut config = DaemonConfig::new();
    /// config.umask(0o022);
    /// ```
    pub fn umask(&mut self, mode: u32) -> &mut Self {
        self.umask = mode;
        self
    }

    /// Sets the stdout redirect file path. Default: none (stays `/dev/null`).
    ///
    /// By default a daemon's stdout is `/dev/null`, so anything written to it
    /// (including `println!`) is discarded silently. Set this to a file path to
    /// capture it. In foreground mode, setting this overrides the inherited
    /// terminal stdout.
    pub fn stdout(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.stdout = Some(path.into());
        self
    }

    /// Sets the stderr redirect file path. Default: none (stays `/dev/null`).
    ///
    /// By default a daemon's stderr is `/dev/null`, so anything written to it
    /// (including `eprintln!` and panic messages) is discarded silently. Set
    /// this to a file path to capture it. In foreground mode, setting this
    /// overrides the inherited terminal stderr.
    pub fn stderr(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.stderr = Some(path.into());
        self
    }

    /// Sets whether to append to stdout/stderr files. Default: `false`.
    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    /// Sets an explicit lockfile path, separate from the pidfile.
    ///
    /// Default: derived — the pidfile (if any) doubles as the lockfile, so a
    /// pidfile alone enforces a single instance. Overrides a previous
    /// [`no_lockfile`](DaemonConfig::no_lockfile) (last call wins).
    pub fn lockfile(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.lockfile = LockfileSetting::Path(path.into());
        self
    }

    /// Disables locking: the pidfile (if any) is written without being
    /// flock'd, and no lockfile is created.
    ///
    /// Use when something else enforces single-instance (a supervisor, a
    /// service manager) or when multiple instances are intended. Overrides a
    /// previous [`lockfile`](DaemonConfig::lockfile) (last call wins).
    pub fn no_lockfile(&mut self) -> &mut Self {
        self.lockfile = LockfileSetting::Disabled;
        self
    }

    /// The lockfile path after resolving the tri-state setting: an explicit
    /// path, the pidfile (derived default), or none (disabled / no pidfile).
    pub(crate) fn effective_lockfile(&self) -> Option<&PathBuf> {
        match &self.lockfile {
            LockfileSetting::Path(path) => Some(path),
            LockfileSetting::DeriveFromPidfile => self.pidfile.as_ref(),
            LockfileSetting::Disabled => None,
        }
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
    /// notification pipe. Stdout and stderr are left inherited (not
    /// redirected to `/dev/null`) unless explicitly configured with
    /// [`stdout`](DaemonConfig::stdout)/[`stderr`](DaemonConfig::stderr).
    /// All other steps (umask, chdir, signal reset, etc.) still execute.
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

    /// Sets whether to remove the pidfile on drop. Default: `true`.
    ///
    /// **Caveat:** `Drop` does not run when the process is killed by a signal
    /// (`SIGTERM`, `SIGINT`, `SIGKILL`, …), which is how daemons are normally
    /// stopped — so with the default the pidfile is still left stale on signal
    /// termination. To remove it on shutdown, call
    /// [`DaemonContext::cleanup_on_term_signals`](crate::DaemonContext::cleanup_on_term_signals)
    /// once, or install a signal handler that exits the main loop cleanly
    /// (letting this context drop) or calls
    /// [`DaemonContext::cleanup`](crate::DaemonContext::cleanup) explicitly. See
    /// the `examples/echo_server.rs` example.
    ///
    /// When `true`, dropping [`DaemonContext`](crate::DaemonContext) removes
    /// the pidfile from disk. Can be overridden at runtime via
    /// [`DaemonContext::set_cleanup_on_drop`](crate::DaemonContext::set_cleanup_on_drop).
    pub fn cleanup_on_drop(&mut self, cleanup: bool) -> &mut Self {
        self.cleanup_on_drop = cleanup;
        self
    }

    /// Sets whether [`drop_privileges`](crate::DaemonContext::drop_privileges)
    /// chowns the configured path-based resources (pidfile, lockfile, stdout,
    /// stderr) to the target user/group before switching. Default: `true`.
    ///
    /// Disable to keep those files owned by the original (privileged) user —
    /// e.g. so a compromised daemon cannot rewrite its own pidfile or truncate
    /// its logs — or to manage ownership yourself with custom users/groups.
    pub fn chown_paths(&mut self, chown: bool) -> &mut Self {
        self.chown_paths = chown;
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
    /// You do **not** need to call this yourself: [`daemonize`](crate::daemonize)
    /// calls it internally before forking. It is exposed so you can validate a
    /// config up front and report errors before daemonizing (e.g. to stderr
    /// while still attached to a terminal).
    ///
    /// Performs minimal I/O: checks path existence, directory writability
    /// (via `faccessat`), and queries the effective UID when a user switch is
    /// configured. No files are created or modified.
    ///
    /// The writability probe passes `AT_EACCESS`, so it answers for the
    /// effective UID/GID — the identity a setuid binary writes as. On Android
    /// bionic rejects that flag, so there the probe answers for the **real**
    /// UID/GID instead. The check is advisory either way: it is inherently racy,
    /// and a writable answer is not a promise the later write succeeds.
    ///
    /// # Errors
    ///
    /// Returns `DaemonizeError::ValidationError` if:
    /// - Any configured path (pidfile, stdout, stderr, lockfile, chdir) contains
    ///   a NUL byte
    /// - Any configured path (pidfile, stdout, stderr, lockfile) is not absolute
    /// - The chdir path is not absolute, does not exist, or is not a directory
    /// - Any configured path that is opened for writing (pidfile, stdout,
    ///   stderr, lockfile) is a directory
    /// - Parent directories of configured paths are not writable — by
    ///   permission, or because the filesystem is read-only — or the probe
    ///   could not answer at all (the errno is named)
    /// - Lockfile or pidfile overlaps with stdout or stderr
    /// - The umask does not fit in the 12 permission bits (`> 0o7777`)
    /// - An environment key is empty or contains `=`
    ///
    /// Returns `DaemonizeError::PermissionDenied` if a user or group is
    /// configured but the effective UID is not 0.
    #[must_use = "validate() returns a Result that must be checked"]
    pub fn validate(&self) -> Result<(), DaemonizeError> {
        // Check chdir is absolute, exists, and is a directory
        validate_path(&self.chdir, "chdir")?;
        if !self.chdir.exists() {
            return Err(DaemonizeError::ValidationError(format!(
                "chdir path does not exist: {}",
                self.chdir.display()
            )));
        }
        if !self.chdir.is_dir() {
            return Err(DaemonizeError::ValidationError(format!(
                "chdir path is not a directory: {}",
                self.chdir.display()
            )));
        }

        // Every path this process writes to, in the order it is checked. An
        // owned file is one the sequence creates and keeps (the pidfile, the
        // lockfile); a stream is one stdout or stderr is redirected into.
        // Adding a writable path means adding a row here, which gives it every
        // per-path check and every overlap check at once — a path that skips
        // one cannot be written (R144).
        let lockfile = self.effective_lockfile();
        let writable = [
            (self.pidfile.as_ref(), "pidfile", Role::Owned),
            (self.stdout.as_ref(), "stdout", Role::Stream),
            (self.stderr.as_ref(), "stderr", Role::Stream),
            // The derived case re-checks the pidfile; harmless.
            (lockfile, "lockfile", Role::Owned),
        ];
        for (path, name, _) in writable {
            if let Some(path) = path {
                validate_path(path, name)?;
                validate_parent_writable(path, name)?;
                reject_directory(path, name)?;
            }
        }

        // No owned file may also be a stream: the redirect's O_TRUNC would
        // empty it. An owned file may equal another owned file (the pidfile is
        // its own lockfile by default), and stdout may share stderr's file.
        // Owned rows come in table order, so a derived lockfile (== pidfile) is
        // reported as the pidfile the user actually configured.
        let rows = |role: Role| {
            writable
                .iter()
                .filter(move |(_, _, r)| *r == role)
                .filter_map(|(path, name, _)| path.map(|p| (p, *name)))
        };
        for (owned, owned_name) in rows(Role::Owned) {
            for (stream, stream_name) in rows(Role::Stream) {
                if paths_same(owned, stream) {
                    return Err(DaemonizeError::ValidationError(format!(
                        "{owned_name} and {stream_name} must not be the same path: {}",
                        owned.display()
                    )));
                }
            }
        }

        // Umask must fit in the 12 permission bits.
        if self.umask & !0o7777 != 0 {
            return Err(DaemonizeError::ValidationError(format!(
                "umask must be <= 0o7777, got {:#o}",
                self.umask
            )));
        }

        // Environment validation: reject exactly what would make
        // `std::env::set_var` panic in the daemon child at step 11 (empty key,
        // '=' or NUL in the key, NUL in the value), so misconfiguration
        // surfaces here as a ValidationError instead of a post-fork panic.
        for (key, value) in &self.env {
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
            if key.contains('\0') || value.contains('\0') {
                return Err(DaemonizeError::ValidationError(format!(
                    "environment entry must not contain a NUL byte: key {key:?}"
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

/// Rejects a configured path that is malformed (contains a NUL byte, which no
/// syscall can accept) or not absolute, so both surface at `validate()` rather
/// than as a late `EINVAL` when the path is first passed to the OS.
fn validate_path(path: &std::path::Path, name: &str) -> Result<(), DaemonizeError> {
    use std::os::unix::ffi::OsStrExt;
    if path.as_os_str().as_bytes().contains(&0) {
        return Err(DaemonizeError::ValidationError(format!(
            "{name} path must not contain a NUL byte: {}",
            path.display()
        )));
    }
    if !path.is_absolute() {
        return Err(DaemonizeError::ValidationError(format!(
            "{name} path must be absolute: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Flags for the parent-directory writability probe.
///
/// `AT_EACCESS` asks `faccessat` to answer for the effective UID/GID, which is
/// what a setuid binary needs to know. Bionic rejects the flag, so `nix` does
/// not define the constant on Android and the probe answers for the real UID
/// there instead; see [`DaemonConfig::validate`] for what that means for
/// callers.
#[cfg(not(blivet_faccessat_lacks_eaccess))]
const EFFECTIVE_ACCESS: nix::fcntl::AtFlags = nix::fcntl::AtFlags::AT_EACCESS;
/// Where the platform rejects the flag, the probe has only the real UID to
/// answer for.
#[cfg(blivet_faccessat_lacks_eaccess)]
const EFFECTIVE_ACCESS: nix::fcntl::AtFlags = nix::fcntl::AtFlags::empty();

/// What a writable path is to the sequence, which decides the overlaps it may
/// not have — see `DaemonConfig::validate`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Created and kept by the sequence: the pidfile, the lockfile.
    Owned,
    /// Redirected into by stdout or stderr.
    Stream,
}

/// Rejects a path that is a directory.
///
/// Runs after the parent-writability check so that `/` — the one absolute path
/// with no parent — keeps reporting the parent problem it always reported.
///
/// Every path this runs on is opened for writing later. Only the pidfile was
/// checked, so pointing `stdout` at a directory passed validation and failed
/// with `EISDIR` at step 12 — in the forked daemon, after step 8 had already
/// written the pidfile. A user's mistake belongs pre-fork, where it is a
/// `ValidationError` and nothing has happened yet.
fn reject_directory(path: &std::path::Path, label: &str) -> Result<(), DaemonizeError> {
    if path.is_dir() {
        return Err(DaemonizeError::ValidationError(format!(
            "{label} path is a directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_parent_writable(path: &std::path::Path, name: &str) -> Result<(), DaemonizeError> {
    use nix::unistd::AccessFlags;

    let parent = path.parent().ok_or_else(|| {
        DaemonizeError::ValidationError(format!(
            "{name} path has no parent directory: {}",
            path.display()
        ))
    })?;
    if !parent.exists() {
        return Err(DaemonizeError::ValidationError(format!(
            "{name} parent directory does not exist: {}",
            parent.display()
        )));
    }
    // Existing and writable are both true of a regular file, and every message
    // here says "parent directory". Without this the config passed validation
    // and then failed at daemonize time with ENOTDIR, reported as "could not
    // create" rather than as the bad path it is.
    if !parent.is_dir() {
        return Err(DaemonizeError::ValidationError(format!(
            "{name} parent is not a directory: {}",
            parent.display()
        )));
    }
    // Writability is probed with faccessat, against the effective UID/GID where
    // the platform supports it (see EFFECTIVE_ACCESS).
    match nix::unistd::faccessat(
        crate::unsafe_ops::at_fdcwd(),
        parent,
        AccessFlags::W_OK,
        EFFECTIVE_ACCESS,
    ) {
        Ok(()) => Ok(()),
        Err(errno) => Err(writability_error(name, parent, errno)),
    }
}

/// Describes a failed writability probe.
///
/// `EACCES` is the answer the probe asked for and is reported as such. Every
/// other errno is named rather than translated, because none of them answers
/// the permission question, and a blanket "not writable" hides that — which is
/// how an `EINVAL` from a flag bionic does not accept reached users as a false
/// permission error.
///
/// Two errnos actually arrive here. `EINVAL` is that rejected flag, and the
/// probe could not answer, so the message says the check failed. `EROFS` is the
/// honest edge: the probe *did* answer — the directory really is unwritable, on
/// a read-only filesystem rather than by permission — so it gets its own arm
/// saying that, which is both determinate and actionable. It is also the common
/// one: `/` is read-only on macOS under SSV, as is any read-only container
/// mount.
///
/// The errnos that describe the path rather than its permissions — `ENOTDIR`,
/// `ELOOP` — cannot reach this function from [`DaemonConfig::validate`]: the
/// parent must exist and be a directory before the probe runs, and
/// `Path::exists` follows symlinks, so a loop answers "does not exist". They
/// are still named rather than translated if they ever arrive, which costs
/// nothing and is what the blanket claim above would have to say anyway.
fn writability_error(
    name: &str,
    parent: &std::path::Path,
    errno: nix::errno::Errno,
) -> DaemonizeError {
    let parent = parent.display();
    DaemonizeError::ValidationError(match errno {
        nix::errno::Errno::EACCES => format!("{name} parent directory is not writable: {parent}"),
        nix::errno::Errno::EROFS => {
            format!("{name} parent directory is on a read-only filesystem (EROFS): {parent}")
        }
        _ => format!("{name} parent directory writability check failed ({errno}): {parent}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Covers: R1
    #[test]
    fn new_equals_default() {
        assert_eq!(DaemonConfig::new(), DaemonConfig::default());
    }

    // Covers: R1 (C-COMMON-TRAITS: Hash consistent with Eq)
    #[test]
    fn equal_configs_hash_equal() {
        let mut a = DaemonConfig::new();
        a.pidfile("/a/x.pid").umask(0o022).env("K", "V");
        let b = a.clone();
        let set: std::collections::HashSet<DaemonConfig> = [a, b].into_iter().collect();
        assert_eq!(set.len(), 1, "equal configs must collapse in a HashSet");
    }

    // Covers: R2, R20, R23
    #[test]
    fn default_values() {
        let config = DaemonConfig::default();
        assert_eq!(config.pidfile, None);
        assert_eq!(config.chdir, PathBuf::from("/"));
        assert_eq!(config.umask, 0);
        assert_eq!(config.stdout, None);
        assert_eq!(config.stderr, None);
        assert!(!config.append);
        assert_eq!(config.lockfile, LockfileSetting::DeriveFromPidfile);
        assert_eq!(config.user, None);
        assert_eq!(config.group, None);
        assert!(!config.foreground);
        assert!(config.close_fds);
        assert!(config.chown_paths);
        assert!(config.env.is_empty());
    }

    #[test]
    fn chown_paths_builder_disables() {
        let mut config = DaemonConfig::new();
        config.chown_paths(false);
        assert!(!config.chown_paths);
    }

    // Covers: R131
    #[test]
    fn lockfile_derives_from_pidfile_by_default() {
        let mut config = DaemonConfig::new();
        config.pidfile("/a/x.pid");
        assert_eq!(
            config.effective_lockfile(),
            Some(&PathBuf::from("/a/x.pid"))
        );
    }

    // Covers: R131
    #[test]
    fn no_pidfile_means_no_derived_lockfile() {
        assert_eq!(DaemonConfig::new().effective_lockfile(), None);
    }

    // Covers: R131
    #[test]
    fn explicit_lockfile_overrides_derivation() {
        let mut config = DaemonConfig::new();
        config.pidfile("/a/x.pid").lockfile("/a/x.lock");
        assert_eq!(
            config.effective_lockfile(),
            Some(&PathBuf::from("/a/x.lock"))
        );
    }

    // Covers: R132
    #[test]
    fn no_lockfile_disables_locking() {
        let mut config = DaemonConfig::new();
        config.pidfile("/a/x.pid").no_lockfile();
        assert_eq!(config.effective_lockfile(), None);
    }

    // Covers: R133
    #[test]
    fn lockfile_after_no_lockfile_wins() {
        let mut config = DaemonConfig::new();
        config.no_lockfile().lockfile("/a/x.lock");
        assert_eq!(
            config.effective_lockfile(),
            Some(&PathBuf::from("/a/x.lock"))
        );
    }

    // Covers: R133
    #[test]
    fn no_lockfile_after_lockfile_wins() {
        let mut config = DaemonConfig::new();
        config.lockfile("/a/x.lock").no_lockfile();
        assert_eq!(config.effective_lockfile(), None);
    }

    // Covers: R84
    #[test]
    fn builder_setters_replace() {
        let mut config = DaemonConfig::new();
        config.pidfile("/a").pidfile("/b");
        assert_eq!(config.pidfile, Some(PathBuf::from("/b")));
    }

    // Covers: R3
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

    /// Every path-shaped validation message names the offending path, so a
    /// consumer with several configured paths knows which value to fix.
    #[test]
    fn validation_errors_include_offending_path() {
        let msg = |config: &DaemonConfig| match config.validate() {
            Err(DaemonizeError::ValidationError(msg)) => msg,
            other => panic!("expected ValidationError, got {other:?}"),
        };

        // The two cases below reach the filesystem: one needs an existing
        // directory to be a pidfile, the other needs a writable parent for the
        // overlap check to be the failure that fires.
        let tmp = crate::test_support::tmp_dir();
        let tmp_name = tmp.display().to_string();
        let overlap = tmp.join("same.pid");
        let overlap_name = overlap.display().to_string();

        // A regular file standing where a parent directory is named. It exists
        // and its owner may write it, so the two questions the probe used to
        // ask both answer yes.
        let parent_file = tmp.join("blivet-parent-is-a-file");
        std::fs::write(&parent_file, b"").unwrap();
        let parent_file_name = parent_file.display().to_string();
        let under_a_file = parent_file.join("daemon.pid");

        type Setup<'a> = &'a dyn Fn(&mut DaemonConfig);
        let cases: [(&str, Setup, &str); 8] = [
            (
                "chdir relative",
                &|c| {
                    c.chdir("relative/path");
                },
                "relative/path",
            ),
            (
                "chdir missing",
                &|c| {
                    c.chdir("/nonexistent_daemonize_test_dir");
                },
                "/nonexistent_daemonize_test_dir",
            ),
            (
                "chdir not a dir",
                &|c| {
                    c.chdir("/etc/hosts");
                },
                "/etc/hosts",
            ),
            (
                "pidfile relative",
                &|c| {
                    c.pidfile("relative.pid");
                },
                "relative.pid",
            ),
            (
                "pidfile is dir",
                &|c| {
                    c.pidfile(&tmp);
                },
                &tmp_name,
            ),
            (
                "stdout parent missing",
                &|c| {
                    c.stdout("/nonexistent_daemonize_test_dir/out.log");
                },
                "/nonexistent_daemonize_test_dir",
            ),
            (
                "pidfile/stdout overlap",
                &|c| {
                    c.pidfile(&overlap).stdout(&overlap);
                },
                &overlap_name,
            ),
            (
                "pidfile parent is a file",
                &|c| {
                    c.pidfile(&under_a_file);
                },
                &parent_file_name,
            ),
        ];
        for (name, setup, expected_path) in cases {
            let mut config = DaemonConfig::new();
            setup(&mut config);
            let m = msg(&config);
            assert!(
                m.contains(expected_path),
                "{name}: message {m:?} does not name the offending path {expected_path:?}"
            );
        }
        let _ = std::fs::remove_file(&parent_file);
    }

    // Covers: R144
    #[test]
    fn validate_rejects_a_directory_where_a_file_is_opened() {
        // The pidfile had this check and the output paths did not, so a plain
        // mistake — pointing -o at a directory — reached step 12 in the forked
        // daemon, where `open` fails with EISDIR after the pidfile is already
        // on disk. Pre-fork is where a user error belongs.
        let dir = tempfile::tempdir().unwrap();
        for label in ["pidfile", "stdout", "stderr", "lockfile"] {
            let mut cfg = DaemonConfig::new();
            match label {
                "pidfile" => cfg.pidfile(dir.path()),
                "stdout" => cfg.stdout(dir.path()),
                "stderr" => cfg.stderr(dir.path()),
                _ => cfg.lockfile(dir.path()),
            };
            match cfg.validate() {
                Err(DaemonizeError::ValidationError(msg)) => assert!(
                    msg.contains("is a directory") && msg.contains(label),
                    "{label}: expected a directory complaint naming it, got {msg}"
                ),
                other => panic!("{label}: a directory was accepted: {other:?}"),
            }
        }
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

    // Covers: R136
    #[test]
    fn validate_rejects_nul_byte_in_path() {
        // A NUL byte makes a path unusable by any syscall (CString::new fails),
        // so validate() must reject it up front rather than letting it surface
        // as a late EINVAL at daemonize time. The parent dir exists, so this
        // isolates the NUL rejection from the parent-writable check.
        let mut config = DaemonConfig::new();
        config.pidfile(crate::test_support::tmp_dir().join("pid\0file"));
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(_))
        ));
    }

    // Covers: R31
    #[test]
    fn validate_pidfile_not_directory() {
        let mut config = DaemonConfig::new();
        config.pidfile(crate::test_support::tmp_dir());
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

    // Covers: R33
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

    // Covers: R33
    #[test]
    fn validate_no_lockfile_pidfile_stdout_overlap_still_rejected() {
        // no_lockfile() removes the lockfile fd, so the lockfile overlap pairs
        // go inert — but the pidfile is still written, so a pidfile == stdout
        // overlap must still be rejected via the pidfile pair.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared");
        let path_str = path.to_str().unwrap();
        let mut config = DaemonConfig::new();
        config.pidfile(path_str).stdout(path_str).no_lockfile();
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
    fn validate_umask_in_range_ok() {
        let mut config = DaemonConfig::new();
        config.umask(0o7777);
        assert!(
            !matches!(&config.validate(), Err(DaemonizeError::ValidationError(msg)) if msg.contains("umask"))
        );
    }

    #[test]
    fn validate_umask_out_of_range_rejected() {
        let mut config = DaemonConfig::new();
        config.umask(0o10000);
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(msg)) if msg.contains("umask")
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

    // Covers: R138
    #[test]
    fn validate_env_key_with_nul_rejected() {
        // std::env::set_var panics on a NUL byte; validate() must reject it up
        // front as a ValidationError, not let the daemon child panic at step 11.
        let mut config = DaemonConfig::new();
        config.env("KEY\0BAD", "value");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(msg)) if msg.contains("NUL")
        ));
    }

    // Covers: R138
    #[test]
    fn validate_env_value_with_nul_rejected() {
        let mut config = DaemonConfig::new();
        config.env("KEY", "val\0ue");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(msg)) if msg.contains("NUL")
        ));
    }

    #[test]
    fn validate_default_config_ok() {
        // Default config should validate (we're not root, no user switch)
        assert!(DaemonConfig::new().validate().is_ok());
    }

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

    // Covers: R83 — Display messages are lowercase with no trailing punctuation.
    #[test]
    fn display_messages_are_lowercase_without_trailing_punctuation() {
        use std::io::{Error, ErrorKind};

        let variants = [
            DaemonizeError::ValidationError("detail".into()),
            DaemonizeError::ProgramNotFound("detail".into()),
            DaemonizeError::UserNotFound("detail".into()),
            DaemonizeError::GroupNotFound("detail".into()),
            DaemonizeError::LockConflict {
                path: "detail".into(),
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
            DaemonizeError::NotifyFailed(Error::from(ErrorKind::BrokenPipe)),
            DaemonizeError::PrivilegesNotDropped,
            DaemonizeError::application(71, "detail"),
        ];

        // Exhaustiveness guard: adding a variant breaks compilation here,
        // forcing it to be added to `variants` above and re-checked.
        fn assert_all_variants_listed(e: &DaemonizeError) {
            match e {
                DaemonizeError::ValidationError(_)
                | DaemonizeError::ProgramNotFound(_)
                | DaemonizeError::UserNotFound(_)
                | DaemonizeError::GroupNotFound(_)
                | DaemonizeError::LockConflict { .. }
                | DaemonizeError::LockfileError(_)
                | DaemonizeError::ForkFailed(_)
                | DaemonizeError::SetsidFailed(_)
                | DaemonizeError::ChdirFailed(_)
                | DaemonizeError::SystemError(_)
                | DaemonizeError::PermissionDenied(_)
                | DaemonizeError::PidfileError(_)
                | DaemonizeError::OutputFileError(_)
                | DaemonizeError::ChownError(_)
                | DaemonizeError::ExecFailed(_)
                | DaemonizeError::NotifyFailed(_)
                | DaemonizeError::PrivilegesNotDropped
                | DaemonizeError::Application { .. } => {}
            }
        }

        for v in &variants {
            assert_all_variants_listed(v);
            let msg = v.to_string();
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

    // Covers: R46, R47, R48
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
    fn derived_lockfile_overlap_reports_pidfile() {
        // Only a pidfile is configured; the derived lockfile also overlaps,
        // but the message must name the path the user actually set.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.log");
        let path_str = path.to_str().unwrap();
        let mut config = DaemonConfig::new();
        config.pidfile(path_str).stdout(path_str);
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(msg)) if msg.starts_with("pidfile and stdout")
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
    fn validate_path_without_parent_rejected() {
        let mut config = DaemonConfig::new();
        config.stdout("/");
        assert!(matches!(
            config.validate(),
            Err(DaemonizeError::ValidationError(msg))
                if msg.contains("no parent directory") && msg.contains('/')
        ));
    }

    #[test]
    fn validate_unwritable_parent_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut config = DaemonConfig::new();
        config.stdout(dir.path().join("out.log"));
        let result = config.validate();
        // Restore before asserting so tempdir cleanup succeeds either way.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        // The probe answers for the effective identity, and root may write a
        // 0555 directory — so the root tier asserts the other half of the same
        // rule rather than asserting nothing.
        if nix::unistd::geteuid().is_root() {
            assert!(
                result.is_ok(),
                "root can write a 0555 directory: {result:?}"
            );
        } else {
            assert!(matches!(
                result,
                Err(DaemonizeError::ValidationError(msg))
                    if msg.contains("not writable") && msg.contains(&dir.path().display().to_string())
            ));
        }
    }

    // Covers: R141
    #[test]
    fn only_eacces_is_reported_as_unwritable() {
        use nix::errno::Errno;

        let parent = std::path::Path::new("/a");
        let denied = writability_error("pidfile", parent, Errno::EACCES).to_string();
        assert!(
            denied.contains("not writable") && denied.contains("/a"),
            "a genuine permission denial must say so and name the path: {denied}"
        );

        // An unsupported flag reaching users as "not writable" is how the
        // Android build break looked from the outside.
        for errno in [Errno::EINVAL, Errno::ENOTDIR, Errno::ELOOP] {
            let msg = writability_error("pidfile", parent, errno).to_string();
            assert!(
                msg.contains(errno.desc()) || msg.contains(&format!("{errno}")),
                "{errno} must be named, not translated into a permission \
                 claim: {msg}"
            );
            assert!(
                !msg.contains("not writable"),
                "{errno} does not answer the permission question, so it must \
                 not be reported as one: {msg}"
            );
            assert!(msg.contains("/a"), "the path must still be named: {msg}");
        }
    }

    /// The writability probe answers for the identity the daemon will write
    /// as: the effective UID wherever `faccessat` accepts `AT_EACCESS`.
    ///
    /// The child drops its *effective* UID while keeping real UID 0, which is
    /// the setuid shape the flag exists for. The two identities then disagree
    /// about a root-owned directory — real UID 0 may write it, effective UID
    /// nobody may not — so `EFFECTIVE_ACCESS` cannot hold the wrong value
    /// without this failing. Asserting the constant instead would only restate
    /// it.
    ///
    /// Root-only and process-global, hence `#[ignore]` and the subprocess: the
    /// root/Linux container tier is what passes `--include-ignored`.
    #[test]
    #[ignore = "drops the process's effective UID; needs root"]
    fn writability_is_probed_against_the_effective_identity() {
        use std::os::unix::fs::PermissionsExt;

        const NAME: &str = "config::tests::writability_is_probed_against_the_effective_identity";
        const ROOT_ONLY_DIR: &str = "__BLIVET_ROOT_ONLY_DIR";
        /// The conventional unprivileged UID; it need not exist in passwd for
        /// `seteuid` to take it.
        const NOBODY: u32 = 65534;

        if let Ok(dir) = std::env::var(ROOT_ONLY_DIR) {
            nix::unistd::seteuid(nix::unistd::Uid::from_raw(NOBODY))
                .expect("dropping the effective UID needs root");
            let mut cfg = DaemonConfig::new();
            cfg.pidfile(std::path::Path::new(&dir).join("daemon.pid"));
            let result = cfg.validate();
            #[cfg(not(blivet_faccessat_lacks_eaccess))]
            assert!(
                matches!(&result, Err(DaemonizeError::ValidationError(msg))
                    if msg.contains("not writable")),
                "the probe must answer for the effective UID, which cannot \
                 write a root-owned directory: {result:?}"
            );
            #[cfg(blivet_faccessat_lacks_eaccess)]
            assert!(
                result.is_ok(),
                "without AT_EACCESS the probe answers for the real UID, which \
                 is root here: {result:?}"
            );
            return;
        }

        if !nix::unistd::geteuid().is_root() {
            eprintln!("skipping: requires root");
            return;
        }

        let dir = crate::test_support::tmp_dir().join("blivet-effective-uid-probe");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).expect("root creates the probe directory");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .expect("root-only permissions");

        let output = crate::test_support::rerun_in_subprocess(NAME, ROOT_ONLY_DIR, &dir);
        let report = crate::test_support::subprocess_report(NAME, &output);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(output.status.success(), "{report}");
    }

    /// The two errnos that reach the probe describe different situations, and
    /// the message has to keep them apart.
    ///
    /// `EROFS` is a determinate answer — the directory really is unwritable —
    /// so a message saying the check *failed* would be wrong, and it is the
    /// message most users meet: `/` is read-only on macOS under SSV, as is any
    /// read-only container mount. `EINVAL` is the indeterminate one: the probe
    /// could not run. Neither is reproducible through `validate`, so they are
    /// tested on the helper that formats them.
    // Covers: R141
    #[test]
    fn a_read_only_filesystem_is_reported_as_the_answer_it_is() {
        let path = std::path::Path::new("/ro");
        let msg = |errno| match writability_error("pidfile", path, errno) {
            DaemonizeError::ValidationError(msg) => msg,
            other => panic!("expected ValidationError, got {other:?}"),
        };

        let erofs = msg(nix::errno::Errno::EROFS);
        assert!(
            erofs.contains("read-only filesystem") && erofs.contains("EROFS"),
            "EROFS determines writability, so say so and name it: {erofs}"
        );
        assert!(
            !erofs.contains("check failed"),
            "the EROFS probe did not fail, it answered: {erofs}"
        );

        let einval = msg(nix::errno::Errno::EINVAL);
        assert!(
            einval.contains("check failed") && einval.contains("EINVAL"),
            "EINVAL means the probe could not answer, and must name itself: {einval}"
        );

        let eacces = msg(nix::errno::Errno::EACCES);
        assert!(
            eacces.contains("is not writable"),
            "EACCES is the answer the probe asked for: {eacces}"
        );
    }

    /// The claim R141 makes about which errnos reach the probe, tested where
    /// the claim is made rather than on the pure helper.
    ///
    /// `ENOTDIR` and `ELOOP` never arrive: the parent must exist and be a
    /// directory first, and `Path::exists` follows symlinks, so a loop answers
    /// "does not exist". `EINVAL` and `EROFS` are the two that do, and neither
    /// is reproducible here — `EINVAL` needs bionic, `EROFS` a read-only mount.
    // Covers: R141
    #[test]
    fn a_path_error_is_answered_before_the_writability_probe() {
        let tmp = crate::test_support::tmp_dir();
        let msg = |config: &DaemonConfig| match config.validate() {
            Err(DaemonizeError::ValidationError(msg)) => msg,
            other => panic!("expected ValidationError, got {other:?}"),
        };

        // A non-final component that is a regular file: ENOTDIR territory.
        let file = tmp.join("blivet-errno-not-a-dir");
        std::fs::write(&file, b"").unwrap();
        let mut config = DaemonConfig::new();
        config.pidfile(file.join("sub").join("daemon.pid"));
        let m = msg(&config);
        let _ = std::fs::remove_file(&file);
        assert!(
            m.contains("does not exist") && !m.contains("check failed"),
            "the existence check must answer first, not the probe: {m}"
        );

        // A symlink loop: ELOOP territory.
        let (a, b) = (
            tmp.join("blivet-errno-loop-a"),
            tmp.join("blivet-errno-loop-b"),
        );
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();
        let mut config = DaemonConfig::new();
        config.pidfile(a.join("daemon.pid"));
        let m = msg(&config);
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
        assert!(
            m.contains("does not exist") && !m.contains("check failed"),
            "a symlink loop must answer as non-existent, not as a probe \
             failure: {m}"
        );
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

    // Covers: R83
    #[test]
    fn display_includes_prefix() {
        let err = DaemonizeError::ValidationError("test message".into());
        assert_eq!(err.to_string(), "validation error: test message");
    }

    // Covers: R37, R38
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
