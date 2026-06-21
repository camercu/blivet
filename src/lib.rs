//! A correct, full-featured Unix daemon library for Rust.
//!
//! A [blivet] is the "impossible fork" optical illusion, also known as the
//! devil's tuning fork. Daemons are created by forking — and this crate
//! performs the impossible double-fork to do it correctly.
//!
//! This crate provides a library and CLI tool for daemonizing processes on Unix
//! systems. It performs a mandatory double-fork, resets signal dispositions and
//! mask, and uses a notification pipe so the parent can wait for daemon
//! readiness. Privilege dropping is split-phase: `daemonize()` returns a
//! context while still privileged, and the caller explicitly calls
//! `drop_privileges()` when ready.
//!
//! [blivet]: https://en.wikipedia.org/wiki/Impossible_trident
//!
//! # Example
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use blivet::{DaemonConfig, daemonize};
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
//! # Choosing an entry point
//!
//! There are two ways to daemonize:
//!
//! - [`daemonize`] is `unsafe`: you must guarantee the process is
//!   single-threaded at the call site (see [Threads and async
//!   runtimes](#threads-and-async-runtimes)). Available on all Unix
//!   platforms.
//! - [`daemonize_checked`] is a safe wrapper that verifies
//!   single-threadedness for you, so no `unsafe` is needed. It is available on
//!   **Linux, macOS, FreeBSD, NetBSD, and OpenBSD**, each using the kernel's
//!   own thread count (`/proc/self/status` on Linux, `proc_pidinfo` on macOS,
//!   `sysctl` on the BSDs). On any other target it is a `#[deprecated]` stub
//!   that never daemonizes — a hard compile error under `-D warnings` /
//!   `#![deny(deprecated)]` — so call `unsafe { daemonize(&config) }` there and
//!   uphold the single-threaded contract yourself.
//!
//! On the mainstream Unixes above you can call it directly:
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let config = blivet::DaemonConfig::new();
//! let mut ctx = blivet::daemonize_checked(&config)?;
//! # ctx.notify_parent()?;
//! # Ok(())
//! # }
//! ```
//!
//! To also compile on an exotic target without thread-count support, gate the
//! call so the deprecated stub is never built:
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let config = blivet::DaemonConfig::new();
//! #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd",
//!           target_os = "netbsd", target_os = "openbsd"))]
//! let mut ctx = blivet::daemonize_checked(&config)?;
//! #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd",
//!               target_os = "netbsd", target_os = "openbsd")))]
//! // SAFETY: no threads spawned before this point.
//! let mut ctx = unsafe { blivet::daemonize(&config)? };
//! # ctx.notify_parent()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Threads and async runtimes
//!
//! Daemonizing forks, and forking a multithreaded process is unsound:
//! mutexes held by other threads stay locked forever in the child. The
//! single-threaded requirement therefore applies **only at fork time**:
//!
//! ```text
//! [single-threaded required]
//!   daemonize() / daemonize_checked()   <- forks here
//!   chown_paths()                        <- still single-threaded
//!   drop_privileges()                    <- still single-threaded (calls setenv)
//!   notify_parent()
//! [now safe to spawn threads / start tokio / accept connections]
//! ```
//!
//! Spawn threads, start an async runtime, or begin a thread-per-connection
//! accept loop **after** [`notify_parent`](DaemonContext::notify_parent).
//! Do not spawn threads between [`daemonize`] and
//! [`drop_privileges`](DaemonContext::drop_privileges): the latter calls
//! `setenv`, which is not thread-safe.
//!
//! # Output and the working directory
//!
//! Two defaults surprise newcomers:
//!
//! - **stdout/stderr go to `/dev/null` by default.** A `println!` after
//!   daemonizing vanishes silently. To capture output, set
//!   [`stdout`](DaemonConfig::stdout) and/or
//!   [`stderr`](DaemonConfig::stderr) to log file paths, or use a logging
//!   crate that writes to a file or syslog.
//! - **the working directory defaults to `/`.** After daemonizing, every
//!   relative path (log files, sockets, config) resolves against `/` and
//!   will usually fail with a confusing "permission denied" or "no such
//!   file". Use absolute paths for all files, or set
//!   [`chdir`](DaemonConfig::chdir) to your desired working directory.
//!
//! # Pidfile cleanup on signals
//!
//! With [`cleanup_on_drop`](DaemonConfig::cleanup_on_drop) (the default),
//! the pidfile is removed when [`DaemonContext`] is dropped — but `Drop`
//! **does not run** when the process is killed by a signal such as
//! `SIGTERM`, which is how daemons are normally stopped. Without a signal
//! handler the pidfile is left stale on disk. See
//! [`DaemonContext::cleanup`] and the `examples/echo_server.rs` example for
//! the recommended pattern.
//!
//! # Exit codes
//!
//! [`DaemonizeError::exit_code`] maps each error to a `sysexits.h` code, but
//! those codes only reach the shell if you use them. The idiomatic
//! `fn main() -> Result<(), E>` prints the error via `Termination` and exits
//! **1**, ignoring `exit_code()`. To preserve the codes, call `exit_code()`
//! yourself:
//!
//! ```no_run
//! use blivet::{daemonize, DaemonConfig, DaemonizeError};
//!
//! fn main() {
//!     if let Err(e) = run() {
//!         eprintln!("{e}");
//!         std::process::exit(e.exit_code() as i32);
//!     }
//! }
//!
//! fn run() -> Result<(), DaemonizeError> {
//!     let config = DaemonConfig::new();
//!     let mut ctx = unsafe { daemonize(&config)? };
//!     // ... application init ...
//!     // notify_parent() returns DaemonizeError, so `?` preserves the exit code.
//!     ctx.notify_parent()?;
//!     Ok(())
//! }
//! ```
//!
//! To report a failure from your own init code (e.g. a socket bind) to the
//! parent with a chosen code, use
//! [`report_error_msg`](DaemonContext::report_error_msg) or the
//! [`DaemonizeError::Application`] variant.
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
//! 1. **Privileged init** — bind sockets, open devices, call
//!    [`chroot`](nix::unistd::chroot), set
//!    [resource limits](nix::sys::resource::setrlimit), or acquire any
//!    other resources that require elevated permissions.
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
//! use blivet::{DaemonConfig, daemonize};
//!
//! let mut config = DaemonConfig::new();
//! config.pidfile("/var/run/foo.pid").user("nobody").group("nogroup");
//!
//! let mut ctx = unsafe { daemonize(&config)? };
//!
//! // 1. Privileged work while still root:
//! //    bind sockets, chroot, set resource limits, etc.
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
mod identity;
pub(crate) mod unsafe_ops;

mod notify;
mod steps;
mod thread_count;
pub(crate) mod util;

#[cfg(test)]
mod test_support;

pub use config::DaemonConfig;
pub use context::DaemonContext;
pub use error::DaemonizeError;

use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};

use nix::unistd::ForkResult;

use forker::{Forker, RealForker};
use notify::NotifyPipe;

/// Daemonize the current process.
///
/// On Linux, prefer the safe wrapper `daemonize_checked`, which verifies
/// the single-threaded requirement for you.
///
/// # Safety
///
/// No other threads may be running when this function is called.
/// Forking a multithreaded process leaves mutexes held by other
/// threads permanently locked in the child, causing deadlocks or
/// undefined behavior. Call before spawning threads, async runtimes,
/// or libraries with background threads. See [Threads and async
/// runtimes](crate#threads-and-async-runtimes) for the full lifecycle.
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

// The OSes where `daemonize_checked` can verify the thread count natively are
// listed explicitly in each `cfg` below (function-like macros do not expand
// inside `cfg` attributes): linux, macos, freebsd, netbsd, openbsd. The count
// itself lives in the `thread_count` module.

/// Safe wrapper for [`daemonize`] that verifies the process is single-threaded.
///
/// Counts the threads in the current process and panics if more than one is
/// running, then calls [`daemonize`]. This upholds the single-threaded
/// contract for you, so no `unsafe` block is needed.
///
/// # Platform support
///
/// Available on **Linux, macOS, FreeBSD, NetBSD, and OpenBSD**, each using the
/// kernel's own thread count (`/proc/self/status` on Linux, `proc_pidinfo` on
/// macOS, `sysctl` on the BSDs). On any other target it is a `#[deprecated]`
/// stub that never daemonizes — calling it warns with guidance (and is a hard
/// compile error under `-D warnings` / `#![deny(deprecated)]`), and panics if
/// invoked anyway; call [`daemonize`] yourself there inside an `unsafe` block.
///
/// # Panics
///
/// Panics if the thread count is greater than 1, or if the thread count cannot
/// be determined.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub fn daemonize_checked(config: &DaemonConfig) -> Result<DaemonContext, DaemonizeError> {
    let count = thread_count::count()
        .expect("daemonize_checked: cannot determine thread count to verify single-threadedness");
    if count > 1 {
        panic!(
            "daemonize_checked: {count} threads running (expected 1). \
             Call daemonize before spawning threads, async runtimes, \
             or libraries with background threads."
        );
    }
    #[allow(unsafe_code)]
    unsafe {
        daemonize(config)
    }
}

/// Stub for targets where the thread count cannot be queried, so there is no
/// safe wrapper to offer.
///
/// Rather than omit the symbol entirely (which yields a bare "cannot find
/// function `daemonize_checked`" error that hides *why*), this stub is provided
/// and marked `#[deprecated]`: using it warns with guidance by default, and is
/// a hard compile error under `-D warnings` / `#![deny(deprecated)]`.
///
/// It never performs an unchecked daemonization. Call
/// `unsafe { `[`daemonize`]`(&config) }` directly on this platform, ensuring
/// the process is single-threaded first.
///
/// # Panics
///
/// Always panics: the operation is unsupported on this target.
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
#[deprecated(
    note = "daemonize_checked cannot verify the thread count on this target. \
            Call `unsafe { daemonize(&config) }` and ensure the process is \
            single-threaded yourself."
)]
pub fn daemonize_checked(_config: &DaemonConfig) -> Result<DaemonContext, DaemonizeError> {
    panic!(
        "daemonize_checked is unsupported on this target (cannot query the thread \
         count); call `unsafe {{ daemonize(&config) }}` and ensure the process is \
         single-threaded yourself"
    )
}

/// Internal daemonization logic, generic over the Forker trait for testability.
#[allow(unsafe_code)]
pub(crate) fn daemonize_inner(
    config: &DaemonConfig,
    forker: &mut impl Forker,
) -> Result<DaemonContext, DaemonizeError> {
    let foreground = config.foreground;

    // Steps 1–3: Fork sequence (skipped in foreground mode)
    let mut pipe_wr = if foreground {
        None
    } else {
        // Step 1: Create notification pipe and first fork
        let pipe = forker.create_notification_pipe();
        let (pipe_rd, mut pipe_wr) = match pipe {
            Some((rd, wr)) => (Some(rd), Some(NotifyPipe::new(wr))),
            None => (None, None),
        };

        // SAFETY: daemonize() is unsafe and requires the caller to ensure
        // the process is single-threaded. daemonize_checked() verifies this
        // on Linux via /proc/self/status before calling daemonize_inner().
        match (unsafe { forker.fork() })? {
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
        if let Err(e) = forker.setsid() {
            signal_error_to_parent(&mut pipe_wr, &e);
            forker.exit(e.exit_code() as i32);
        }

        // Step 3: Second fork
        // SAFETY: same as above — single-threaded post-fork child.
        match unsafe { forker.fork() } {
            Ok(ForkResult::Parent { .. }) => {
                // Intermediate child exits
                drop(pipe_wr);
                forker.exit(0);
            }
            Ok(ForkResult::Child) => {
                // Grandchild continues
            }
            Err(e) => {
                signal_error_to_parent(&mut pipe_wr, &e);
                forker.exit(e.exit_code() as i32);
            }
        }

        pipe_wr
    };

    // Steps 4–14 run in the final daemon process and report failures as a
    // single Result. Funnel any error through one place: notify the parent and
    // exit. `pipe_wr` stays available so the error path can still signal it.
    match run_post_fork(config, &mut pipe_wr) {
        Ok(ctx) => Ok(ctx),
        Err(e) => {
            signal_error_to_parent(&mut pipe_wr, &e);
            forker.exit(e.exit_code() as i32);
        }
    }
}

/// Steps 4–14: apply the configuration in the final daemon process.
///
/// Forker-free and fallible: every step returns its error rather than touching
/// the notification pipe, leaving the single error-to-parent seam in
/// [`daemonize_inner`]. On success the notification pipe write end is moved into
/// the returned [`DaemonContext`]; `pipe_wr` is left `None`.
fn run_post_fork(
    config: &DaemonConfig,
    pipe_wr: &mut Option<NotifyPipe>,
) -> Result<DaemonContext, DaemonizeError> {
    // Step 4: Set umask
    steps::set_umask(config.umask);

    // Step 5: chdir
    steps::change_dir(&config.chdir)?;

    // Step 6: Redirect stdin to /dev/null (always); redirect stdout/stderr
    // to /dev/null only when not in foreground mode (foreground leaves them
    // inherited so output reaches the terminal or supervisor).
    steps::redirect_to_devnull(!config.foreground);

    // Step 7: Open and lock lockfile
    let lockfile = match config.lockfile.as_ref() {
        Some(path) => Some(steps::open_and_lock(path)?),
        None => None,
    };

    // Step 8: Write pidfile
    if let Some(ref pidfile_path) = config.pidfile {
        steps::write_pidfile(
            pidfile_path,
            config.lockfile.as_deref().zip(lockfile.as_ref()),
        )?;
    }

    // Step 9: Reset signal dispositions
    unsafe_ops::reset_signal_dispositions();

    // Step 10: Clear signal mask
    steps::clear_signal_mask();

    // Step 11: Set environment variables
    steps::set_env_vars(&config.env);

    // Step 12: Redirect stdout/stderr to configured files
    if config.stdout.is_some() || config.stderr.is_some() {
        steps::redirect_output(
            config.stdout.as_deref(),
            config.stderr.as_deref(),
            config.append,
        )?;
    }

    // Step 13: Close inherited fds (if enabled)
    if config.close_fds {
        let mut skip_fds: Vec<i32> = Vec::new();
        if let Some(ref flock) = lockfile {
            skip_fds.push(flock.as_raw_fd());
        }
        if let Some(ref wr) = pipe_wr {
            skip_fds.push(wr.as_fd().as_raw_fd());
        }
        steps::close_inherited_fds(&skip_fds);
    }

    // Step 14: Return DaemonContext (clones the config-derived fields it needs)
    Ok(DaemonContext::new(config, lockfile, pipe_wr.take()))
}

/// Parent-side pipe reader. Reads from the pipe and exits accordingly.
fn parent_pipe_reader(rd: OwnedFd, forker: &impl Forker) -> ! {
    let mut file = std::fs::File::from(rd);
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);

    match notify::decode(&buf) {
        notify::Outcome::Success => forker.exit(0),
        notify::Outcome::Failure { code, message } => {
            eprintln!("{message}");
            forker.exit(code);
        }
    }
}

/// Report a daemonization error to the parent via the notification pipe
/// (best-effort), consuming the write end if present. Used by the fork-sequence
/// and post-fork error paths that abort with `forker.exit` immediately after.
fn signal_error_to_parent(pipe_wr: &mut Option<NotifyPipe>, err: &DaemonizeError) {
    if let Some(pipe) = pipe_wr.take() {
        pipe.signal_error(err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forker::null_forker::NullForker;
    use crate::test_support::{is_subprocess, run_in_subprocess};
    use std::panic::catch_unwind;

    #[test]
    fn both_forks_child_succeeds() {
        run_in_subprocess("tests::both_forks_child_succeeds_subprocess");
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
    fn signal_error_to_parent_noop_with_none() {
        signal_error_to_parent(&mut None, &DaemonizeError::ForkFailed("test".into()));
    }

    #[test]
    fn signal_error_to_parent_writes_protocol() {
        let (rd, wr) = nix::unistd::pipe().unwrap();
        let mut pipe_wr = Some(NotifyPipe::new(wr));
        let err = DaemonizeError::ForkFailed("test error".into());
        signal_error_to_parent(&mut pipe_wr, &err);
        assert!(pipe_wr.is_none(), "write end consumed after signalling");

        let mut file = std::fs::File::from(rd);
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();

        assert_eq!(buf[0], 71); // EX_OSERR
        assert_eq!(
            std::str::from_utf8(&buf[1..]).unwrap(),
            "fork failed: test error"
        );
    }

    #[test]
    fn foreground_mode_skips_fork() {
        run_in_subprocess("tests::foreground_mode_skips_fork_subprocess");
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
        run_in_subprocess("tests::foreground_mode_notify_parent_noop_subprocess");
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
        run_in_subprocess("tests::close_fds_false_preserves_fds_subprocess");
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
        run_in_subprocess("tests::context_carries_config_fields_subprocess");
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
