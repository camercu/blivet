//! Daemonize a process using the double-fork method.
//!
//! This crate provides a library and CLI tool for daemonizing processes on Unix
//! systems. It performs a mandatory double-fork, resets signal dispositions and
//! mask, and uses a notification pipe so the parent can wait for daemon
//! readiness. Privilege dropping is split-phase: `daemonize()` returns a
//! context while still privileged, and the caller explicitly calls
//! `drop_privileges()` when ready.
//!
//! # Example
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use daemonize::{DaemonConfig, daemonize};
//!
//! let mut config = DaemonConfig::new();
//! config.pidfile("/var/run/foo.pid").chdir("/tmp");
//!
//! let mut ctx = unsafe { daemonize(&config)? };
//! // ... application initialization ...
//! ctx.notify_parent()?;
//! // daemon process continues here
//! # Ok(())
//! # }
//! ```
//!
//! # Split-phase design
//!
//! Many daemons need root privileges during startup — binding to a
//! privileged port, writing a pidfile to `/var/run`, opening log files
//! owned by root — but should run as an unprivileged user afterward.
//!
//! Rather than coupling privilege dropping into the daemonization call
//! (which would force callers to choose between "drop before init" and
//! "never drop at all"), `daemonize()` returns a [`DaemonContext`] while
//! the process is still running as the original user.  The caller
//! performs any privileged work, then explicitly calls
//! [`chown_paths()`](DaemonContext::chown_paths) and
//! [`drop_privileges()`](DaemonContext::drop_privileges) when ready.
//! Finally, [`notify_parent()`](DaemonContext::notify_parent) signals
//! the original parent that the daemon is up, allowing the parent to
//! exit with a meaningful status.
//!
//! This split gives full control over ordering:
//!
//! 1. **Privileged init** — bind sockets, open devices, acquire
//!    resources that require elevated permissions.
//! 2. **Ownership transfer** — `chown_paths()` hands pidfile, lockfile,
//!    and log files to the target user/group while still root.
//! 3. **Privilege drop** — `drop_privileges()` calls `initgroups`,
//!    `setgid`, and `setuid`.  After this point the process runs as the
//!    configured unprivileged user.
//! 4. **Readiness signal** — `notify_parent()` writes a success byte to
//!    the notification pipe; the parent reads it and exits 0.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use daemonize::{DaemonConfig, daemonize};
//!
//! let mut config = DaemonConfig::new();
//! config.pidfile("/var/run/foo.pid").user("nobody").group("nogroup");
//!
//! let mut ctx = unsafe { daemonize(&config)? };
//!
//! // 1. Privileged work while still root
//! let _listener = std::net::TcpListener::bind("0.0.0.0:80")?;
//!
//! // 2–3. Transfer file ownership, then drop to unprivileged user
//! ctx.chown_paths()?;
//! ctx.drop_privileges()?;
//!
//! // 4. Tell the parent we're ready
//! ctx.notify_parent()?;
//!
//! // Daemon continues as "nobody" with the socket still open
//! # Ok(())
//! # }
//! ```

#![deny(unsafe_code)]

mod config;
mod context;
mod error;
pub(crate) mod forker;
pub(crate) mod unsafe_ops;

mod steps;
pub(crate) mod util;

pub use config::DaemonConfig;
pub use context::DaemonContext;
pub use error::DaemonizeError;

use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};

use nix::unistd::ForkResult;

use forker::Forker;
use unsafe_ops::RealForker;

/// Daemonize the current process.
///
/// # Safety
///
/// No other threads may be running when this function is called.
/// Forking a multithreaded process leaves mutexes held by other
/// threads permanently locked in the child, causing deadlocks or
/// undefined behavior. Call before spawning threads, async runtimes,
/// or libraries with background threads.
///
/// # Errors
///
/// Returns `DaemonizeError` on validation failure or any syscall error
/// during the daemonization sequence. Pre-fork errors are returned
/// directly; post-fork errors are reported via the notification pipe.
///
/// # Panics
///
/// Panics if `/dev/null` cannot be opened, `dup2` to a standard fd fails,
/// `sigprocmask` fails, `getrlimit` fails, or other OS-level invariants
/// are violated (indicating a fundamentally broken environment).
#[allow(unsafe_code)]
pub unsafe fn daemonize(config: &DaemonConfig) -> Result<DaemonContext, DaemonizeError> {
    config.validate()?;
    daemonize_inner(config, &mut RealForker)
}

/// Safe wrapper for [`daemonize`] that verifies the process is single-threaded.
///
/// Reads `/proc/self/status` and parses the `Threads:` line. If the thread
/// count exceeds 1, or if `/proc/self/status` cannot be read or parsed,
/// this function panics.
///
/// # Panics
///
/// Panics if the thread count is greater than 1, or if `/proc/self/status`
/// is unavailable or unparseable.
#[cfg(target_os = "linux")]
pub fn daemonize_checked(config: &DaemonConfig) -> Result<DaemonContext, DaemonizeError> {
    let status = std::fs::read_to_string("/proc/self/status")
        .expect("failed to read /proc/self/status: cannot verify thread count");
    let threads = status
        .lines()
        .find(|line| line.starts_with("Threads:"))
        .expect("failed to find Threads: line in /proc/self/status");
    let count: usize = threads
        .split_whitespace()
        .nth(1)
        .expect("malformed Threads: line in /proc/self/status")
        .parse()
        .expect("failed to parse thread count from /proc/self/status");
    if count > 1 {
        panic!(
            "daemonize_checked: {} threads running (expected 1). \
             Call daemonize before spawning threads, async runtimes, \
             or libraries with background threads.",
            count
        );
    }
    #[allow(unsafe_code)]
    unsafe {
        daemonize(config)
    }
}

/// Internal daemonization logic, generic over the Forker trait for testability.
pub(crate) fn daemonize_inner(
    config: &DaemonConfig,
    forker: &mut impl Forker,
) -> Result<DaemonContext, DaemonizeError> {
    let foreground = config.get_foreground();

    // Steps 1–3: Fork sequence (skipped in foreground mode)
    let pipe_wr = if foreground {
        None
    } else {
        // Step 1: Create notification pipe and first fork
        let pipe = forker.create_notification_pipe();
        let (pipe_rd, pipe_wr) = match pipe {
            Some((rd, wr)) => (Some(rd), Some(wr)),
            None => (None, None),
        };

        match forker.fork()? {
            ForkResult::Parent { .. } => {
                // Parent: close write end, read from pipe, exit
                drop(pipe_wr);
                if let Some(rd) = pipe_rd {
                    parent_pipe_reader(rd, forker);
                }
                // If no pipe (NullForker), just exit
                forker.exit(0);
            }
            ForkResult::Child => {
                // Child: close read end, continue
                drop(pipe_rd);
            }
        }

        // Step 2: setsid
        let pipe_wr_ref = &pipe_wr;
        if let Err(e) = forker.setsid() {
            write_error_to_pipe(pipe_wr_ref, &e);
            forker.exit(e.exit_code() as i32);
        }

        // Step 3: Second fork
        match forker.fork() {
            Ok(ForkResult::Parent { .. }) => {
                // Intermediate child exits
                drop(pipe_wr);
                forker.exit(0);
            }
            Ok(ForkResult::Child) => {
                // Grandchild continues
            }
            Err(e) => {
                write_error_to_pipe(pipe_wr_ref, &e);
                forker.exit(e.exit_code() as i32);
            }
        }

        pipe_wr
    };

    // Macro for post-fork error handling: write to pipe + exit
    macro_rules! post_fork_try {
        ($result:expr) => {
            match $result {
                Ok(val) => val,
                Err(e) => {
                    write_error_to_pipe(&pipe_wr, &e);
                    forker.exit(e.exit_code() as i32);
                }
            }
        };
    }

    // Step 4: Set umask
    steps::set_umask(config.get_umask());

    // Step 5: chdir
    post_fork_try!(steps::change_dir(config.get_chdir()));

    // Step 6: Redirect stdin/stdout/stderr to /dev/null
    steps::redirect_to_devnull();

    // Step 7: Open and lock lockfile (match required: macro uses divergent control flow)
    #[allow(clippy::manual_map)]
    let lockfile = match config.get_lockfile() {
        Some(path) => Some(post_fork_try!(steps::open_and_lock(path))),
        None => None,
    };

    // Step 8: Write pidfile
    if let Some(pidfile_path) = config.get_pidfile() {
        post_fork_try!(steps::write_pidfile(
            pidfile_path,
            config.get_lockfile(),
            lockfile.as_ref()
        ));
    }

    // Step 9: Reset signal dispositions
    unsafe_ops::reset_signal_dispositions();

    // Step 10: Clear signal mask
    steps::clear_signal_mask();

    // Step 11: Set environment variables
    steps::set_env_vars(config.get_env());

    // Step 12: Redirect stdout/stderr to configured files
    if config.get_stdout().is_some() || config.get_stderr().is_some() {
        post_fork_try!(steps::redirect_output(
            config.get_stdout(),
            config.get_stderr(),
            config.get_append(),
        ));
    }

    // Step 13: Close inherited fds (if enabled)
    if config.get_close_fds() {
        let mut skip_fds: Vec<i32> = Vec::new();
        if let Some(ref flock) = lockfile {
            skip_fds.push(flock.as_raw_fd());
        }
        if let Some(ref wr) = pipe_wr {
            skip_fds.push(wr.as_raw_fd());
        }
        steps::close_inherited_fds(&skip_fds);
    }

    // Step 14: Return DaemonContext with cloned config fields
    Ok(DaemonContext::new(
        lockfile,
        pipe_wr,
        config.get_pidfile().cloned(),
        config.get_lockfile().cloned(),
        config.get_stdout().cloned(),
        config.get_stderr().cloned(),
        config.get_user().map(String::from),
        config.get_group().map(String::from),
    ))
}

/// Parent-side pipe reader. Reads from the pipe and exits accordingly.
fn parent_pipe_reader(rd: OwnedFd, forker: &impl Forker) -> ! {
    let mut file = std::fs::File::from(rd);
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);

    if buf.is_empty() {
        // EOF: exec succeeded (CLOEXEC closed pipe)
        forker.exit(0);
    }

    let code = buf[0];
    if code == 0x00 {
        // Success byte
        forker.exit(0);
    }

    // Error: code byte + message
    let msg = String::from_utf8_lossy(&buf[1..]);
    eprintln!("{msg}");
    forker.exit(code as i32);
}

/// Write error protocol to notification pipe (best-effort).
fn write_error_to_pipe(pipe_wr: &Option<OwnedFd>, err: &DaemonizeError) {
    if let Some(ref fd) = pipe_wr {
        // Write directly via borrowed fd to avoid consuming the OwnedFd
        let msg = err.to_string();
        let code = err.exit_code();
        let mut buf = Vec::with_capacity(1 + msg.len());
        buf.push(code);
        buf.extend_from_slice(msg.as_bytes());
        let _ = nix::unistd::write(fd, &buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forker::null_forker::NullForker;
    use std::panic::catch_unwind;

    /// Run a `#[ignore]` test in an isolated subprocess.
    ///
    /// Tests that redirect fds or close inherited fds destroy the test harness.
    /// This helper re-invokes the test binary targeting a single `#[ignore]`
    /// test, with an env-var gate so it only runs when spawned from here.
    fn run_subprocess(test_name: &str) {
        let exe = std::env::current_exe().unwrap();
        let status = std::process::Command::new(exe)
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env("__DAEMONIZE_SUBPROCESS_TEST", "1")
            .status()
            .unwrap();
        assert!(status.success(), "subprocess test failed: {status}");
    }

    fn is_subprocess() -> bool {
        std::env::var("__DAEMONIZE_SUBPROCESS_TEST").is_ok()
    }

    #[test]
    fn both_forks_child_succeeds() {
        run_subprocess("tests::both_forks_child_succeeds_subprocess");
    }

    #[test]
    #[ignore]
    fn both_forks_child_succeeds_subprocess() {
        if !is_subprocess() {
            return;
        }
        let mut config = DaemonConfig::new();
        config.close_fds(false); // Don't close fds in test subprocess (systemd aborts on EBADF)
        let mut forker = NullForker::both_child();
        let result = daemonize_inner(&config, &mut forker);
        assert!(result.is_ok());
    }

    #[test]
    fn first_fork_parent_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::first_parent();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            daemonize_inner(&config, &mut forker)
        }));
        assert!(result.is_err()); // exit panics in NullForker
    }

    #[test]
    fn second_fork_parent_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::second_parent();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            daemonize_inner(&config, &mut forker)
        }));
        assert!(result.is_err());
    }

    #[test]
    fn first_fork_fails_returns_error() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::first_fork_fails();
        let result = daemonize_inner(&config, &mut forker);
        assert!(matches!(result, Err(DaemonizeError::ForkFailed(_))));
    }

    #[test]
    fn setsid_fails_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::setsid_fails();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            daemonize_inner(&config, &mut forker)
        }));
        assert!(result.is_err());
    }

    #[test]
    fn second_fork_fails_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::second_fork_fails();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            daemonize_inner(&config, &mut forker)
        }));
        assert!(result.is_err());
    }

    #[test]
    fn exit_panic_contains_code() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::first_parent();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            daemonize_inner(&config, &mut forker)
        }));
        let panic_msg = result
            .unwrap_err()
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(
            panic_msg.contains("NullForker::exit(0)"),
            "panic message should contain exit code, got: {panic_msg}"
        );
    }

    #[test]
    fn write_error_to_pipe_noop_with_none() {
        write_error_to_pipe(&None, &DaemonizeError::ForkFailed("test".into()));
    }

    #[test]
    fn write_error_to_pipe_writes_protocol() {
        let (rd, wr) = nix::unistd::pipe().unwrap();
        let pipe_wr = Some(wr);
        let err = DaemonizeError::ForkFailed("test error".into());
        write_error_to_pipe(&pipe_wr, &err);
        drop(pipe_wr);

        let mut file = std::fs::File::from(rd);
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();

        assert_eq!(buf[0], 71); // EX_OSERR
        assert_eq!(std::str::from_utf8(&buf[1..]).unwrap(), "test error");
    }

    #[test]
    fn foreground_mode_skips_fork() {
        run_subprocess("tests::foreground_mode_skips_fork_subprocess");
    }

    #[test]
    #[ignore]
    fn foreground_mode_skips_fork_subprocess() {
        if !is_subprocess() {
            return;
        }
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false);
        let mut forker = NullForker::new(vec![], Ok(()));
        let result = daemonize_inner(&config, &mut forker);
        let ctx = result.expect("foreground daemonize_inner should succeed");
        assert!(ctx.lockfile_fd().is_none());
    }

    #[test]
    fn foreground_mode_notify_parent_noop() {
        run_subprocess("tests::foreground_mode_notify_parent_noop_subprocess");
    }

    #[test]
    #[ignore]
    fn foreground_mode_notify_parent_noop_subprocess() {
        if !is_subprocess() {
            return;
        }
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false);
        let mut forker = NullForker::new(vec![], Ok(()));
        let mut ctx = daemonize_inner(&config, &mut forker).unwrap();
        assert!(ctx.notify_parent().is_ok());
    }

    #[test]
    fn close_fds_false_preserves_fds() {
        run_subprocess("tests::close_fds_false_preserves_fds_subprocess");
    }

    #[test]
    #[ignore]
    fn close_fds_false_preserves_fds_subprocess() {
        if !is_subprocess() {
            return;
        }
        let (rd, wr) = nix::unistd::pipe().unwrap();

        let mut config = DaemonConfig::new();
        config.close_fds(false);
        let mut forker = NullForker::both_child();
        let _ctx = daemonize_inner(&config, &mut forker).unwrap();

        assert!(
            nix::unistd::write(&wr, b"alive").is_ok(),
            "write fd should still be open with close_fds=false"
        );
        let mut buf = [0u8; 5];
        assert!(
            nix::unistd::read(&rd, &mut buf).is_ok(),
            "read fd should still be open with close_fds=false"
        );
    }

    #[test]
    fn context_carries_config_fields() {
        run_subprocess("tests::context_carries_config_fields_subprocess");
    }

    #[test]
    #[ignore]
    fn context_carries_config_fields_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        let stdout = dir.path().join("out.log");

        let mut config = DaemonConfig::new();
        config
            .pidfile(&pidfile)
            .stdout(&stdout)
            .user("nobody")
            .group("nogroup")
            .foreground(true)
            .close_fds(false);

        let mut forker = NullForker::new(vec![], Ok(()));
        let ctx = daemonize_inner(&config, &mut forker).unwrap();

        let debug = format!("{:?}", ctx);
        assert!(
            debug.contains("test.pid"),
            "context should contain pidfile path"
        );
        assert!(
            debug.contains("out.log"),
            "context should contain stdout path"
        );
        assert!(debug.contains("nobody"), "context should contain user");
        assert!(debug.contains("nogroup"), "context should contain group");
    }
}
