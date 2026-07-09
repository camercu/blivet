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
//! let mut ctx = daemonize(&config)?;
//! // ... application initialization ...
//! ctx.notify_parent()?;
//! // daemon process continues here
//! # Ok(())
//! # }
//! ```
//!
//! # Choosing an entry point
//!
//! There are two entry points:
//!
//! - [`daemonize`] is the safe default: it verifies the process is
//!   single-threaded for you, so no `unsafe` is needed. It is available on
//!   **Linux, macOS, FreeBSD, NetBSD, and OpenBSD**, each using the kernel's
//!   own thread count (`/proc/self/status` on Linux, `proc_pidinfo` on macOS,
//!   `sysctl` on the BSDs). On any other target it is a `#[deprecated]` stub
//!   that never daemonizes — a hard compile error under `-D warnings` /
//!   `#![deny(deprecated)]`; use [`daemonize_unchecked`] there.
//! - [`daemonize_unchecked`] is `unsafe` and available on all Unix platforms:
//!   you must guarantee the process is single-threaded at the call site (see
//!   [Threads and async runtimes](#threads-and-async-runtimes)).
//!
//! Most callers want [`daemonize`]:
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let config = blivet::DaemonConfig::new();
//! let mut ctx = blivet::daemonize(&config)?;
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
//! let mut ctx = blivet::daemonize(&config)?;
//! #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd",
//!               target_os = "netbsd", target_os = "openbsd")))]
//! // SAFETY: no threads spawned before this point.
//! let mut ctx = unsafe { blivet::daemonize_unchecked(&config)? };
//! # ctx.notify_parent()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Threads and async runtimes
//!
//! Daemonizing forks, and forking a multithreaded process is unsound:
//! mutexes held by other threads stay locked forever in the child. A second
//! thread-unsafe step follows:
//! [`drop_privileges`](DaemonContext::drop_privileges) calls `setenv`
//! (`USER`/`HOME`/`LOGNAME`) when switching users. The single-threaded window
//! therefore runs from the fork through the last `setenv` — i.e. through
//! `drop_privileges()`:
//!
//! ```text
//! [single-threaded required]
//!   daemonize() / daemonize_unchecked() <- forks here
//!   chown_paths()                        <- still single-threaded
//!   drop_privileges()                    <- last unsafe step: setenv (USER/HOME/LOGNAME)
//! [now safe to spawn threads / start tokio / accept connections]
//!   notify_parent()                      <- thread-safe; writes one byte to the pipe
//! ```
//!
//! Both guards check for you and panic if violated: [`daemonize`] at the fork,
//! [`drop_privileges`](DaemonContext::drop_privileges) at its `setenv` (when a
//! user is configured). [`daemonize_unchecked`] and
//! [`drop_privileges_unchecked`](DaemonContext::drop_privileges_unchecked) are
//! the `unsafe` opt-outs.
//!
//! Spawn threads, start an async runtime, or begin a thread-per-connection
//! accept loop **after** `drop_privileges()` returns — or after [`daemonize`]
//! returns if you don't switch users.
//! [`notify_parent`](DaemonContext::notify_parent) itself is thread-safe.
//!
//! # Output and the working directory
//!
//! Two defaults match standard daemon behavior (as in `daemonize(1)`), worth
//! keeping in mind:
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
//! # Signals
//!
//! Daemonization resets every signal disposition to its default and clears
//! the signal mask — with one exception: **SIGPIPE is preserved**. The Rust
//! runtime ignores SIGPIPE so writes to a closed pipe or socket return
//! [`ErrorKind::BrokenPipe`](std::io::ErrorKind::BrokenPipe) instead of
//! killing the process, and that guarantee survives [`daemonize`]. (The
//! `daemonize` CLI restores the default disposition just before `exec`, so
//! spawned programs still start with conventional signal state.)
//!
//! # Pidfile cleanup on signals
//!
//! With [`cleanup_on_drop`](DaemonConfig::cleanup_on_drop) (the default),
//! the pidfile is removed when [`DaemonContext`] is dropped — but `Drop`
//! **does not run** when the process is killed by a signal such as
//! `SIGTERM`, which is how daemons are normally stopped. Without a signal
//! handler the pidfile is left stale on disk. Two supported fixes:
//!
//! - [`DaemonContext::cleanup_on_term_signals`] — one call installs
//!   async-signal-safe handlers that unlink the pidfile and re-raise.
//!   Simplest, no extra dependency, but the process still dies mid-flight.
//! - Run your own signal loop for graceful shutdown and let the context
//!   drop (or call [`DaemonContext::cleanup`]) on the way out — see the
//!   `examples/echo_server.rs` example.
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
//!     let mut ctx = daemonize(&config)?;
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
//! let mut ctx = daemonize(&config)?;
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

/// Compile-checks every `rust` code block in the README as a doctest, so a
/// stale snippet fails `cargo test` instead of misleading readers. Blocks
/// that daemonize are marked `no_run`; fragments that cannot stand alone are
/// marked `ignore`. Does not affect the docs.rs front page.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme_doctests {}

pub use config::DaemonConfig;
pub use context::DaemonContext;
pub use error::DaemonizeError;

use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};

use nix::unistd::ForkResult;

use forker::{Forker, RealForker};
use notify::NotifyPipe;

/// Daemonize the current process without verifying the thread count.
///
/// Prefer the safe [`daemonize`], which verifies the single-threaded
/// requirement for you on the mainstream Unixes. Reach for this `unsafe`
/// variant only on targets where [`daemonize`] is unavailable, or when you
/// must manage the single-threaded contract yourself.
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
/// In foreground mode every error is returned directly — the library
/// never exits the caller's process.
///
/// # Panics
///
/// Panics if `/dev/null` cannot be opened, `dup2` to a standard fd fails,
/// `sigprocmask` fails, `getrlimit` fails, or other OS-level invariants
/// are violated (indicating a fundamentally broken environment).
#[allow(unsafe_code)]
pub unsafe fn daemonize_unchecked(config: &DaemonConfig) -> Result<DaemonContext, DaemonizeError> {
    config.validate()?;
    // SAFETY: this `unsafe fn`'s own contract requires single-threadedness.
    unsafe { daemonize_inner(config, &mut RealForker) }
}

// The OSes where `daemonize` can verify the thread count natively are
// listed explicitly in each `cfg` below (function-like macros do not expand
// inside `cfg` attributes): linux, macos, freebsd, netbsd, openbsd. The count
// itself lives in the `thread_count` module.

/// Returns the panic message if `count` is not exactly one thread, else `None`.
///
/// The checked entry points require *exactly* one thread (R45): forking — or
/// calling `setenv` during `drop_privileges` — in a multi-threaded process is
/// unsound. Any count other than 1 is a violation — including an anomalous `0`,
/// which a healthy process can never report and so signals an unreliable
/// thread-count query. Failing closed keeps the safety guard from
/// green-lighting on a count it cannot trust. `caller` names the operation in
/// the panic message.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub(crate) fn single_threaded_violation(caller: &str, count: usize) -> Option<String> {
    (count != 1).then(|| {
        format!(
            "{caller}: {count} threads running (expected 1). \
             Call {caller} before spawning threads, async runtimes, \
             or libraries with background threads."
        )
    })
}

/// Reads the current thread count and panics unless it is exactly 1, naming
/// `caller` in the message.
///
/// The imperative shell around [`single_threaded_violation`], shared by the
/// checked entry points that must run single-threaded: [`daemonize`] (before
/// the fork) and
/// [`DaemonContext::drop_privileges`](crate::DaemonContext::drop_privileges)
/// (before its `setenv`).
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub(crate) fn assert_single_threaded(caller: &str) {
    let count = thread_count::count().unwrap_or_else(|_| {
        panic!("{caller}: cannot determine thread count to verify single-threadedness")
    });
    if let Some(msg) = single_threaded_violation(caller, count) {
        panic!("{msg}");
    }
}

/// Daemonize the current process, verifying it is single-threaded first.
///
/// Counts the threads in the current process and panics unless exactly one is
/// running, then calls [`daemonize_unchecked`]. This upholds the
/// single-threaded contract for you, so no `unsafe` block is needed, and is the
/// recommended entry point.
///
/// # Platform support
///
/// Available on **Linux, macOS, FreeBSD, NetBSD, and OpenBSD**, each using the
/// kernel's own thread count (`/proc/self/status` on Linux, `proc_pidinfo` on
/// macOS, `sysctl` on the BSDs). On any other target it is a `#[deprecated]`
/// stub that never daemonizes — calling it warns with guidance (and is a hard
/// compile error under `-D warnings` / `#![deny(deprecated)]`), and panics if
/// invoked anyway; call [`daemonize_unchecked`] yourself there inside an
/// `unsafe` block.
///
/// # Errors
///
/// As [`daemonize_unchecked`]: `DaemonizeError` on validation failure or any
/// syscall error during the daemonization sequence.
///
/// # Panics
///
/// Panics if the thread count is anything other than exactly 1, or if the
/// thread count cannot be determined.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub fn daemonize(config: &DaemonConfig) -> Result<DaemonContext, DaemonizeError> {
    assert_single_threaded("daemonize");
    #[allow(unsafe_code)]
    unsafe {
        daemonize_unchecked(config)
    }
}

/// Stub for targets where the thread count cannot be queried, so there is no
/// safe wrapper to offer.
///
/// Rather than omit the symbol entirely (which yields a bare "cannot find
/// function `daemonize`" error that hides *why*), this stub is provided
/// and marked `#[deprecated]`: using it warns with guidance by default, and is
/// a hard compile error under `-D warnings` / `#![deny(deprecated)]`.
///
/// It never performs an unchecked daemonization. Call
/// `unsafe { `[`daemonize_unchecked`]`(&config) }` directly on this platform,
/// ensuring the process is single-threaded first.
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
#[deprecated(note = "daemonize cannot verify the thread count on this target. \
            Call `unsafe { daemonize_unchecked(&config) }` and ensure the process \
            is single-threaded yourself.")]
pub fn daemonize(_config: &DaemonConfig) -> Result<DaemonContext, DaemonizeError> {
    panic!(
        "daemonize is unsupported on this target (cannot query the thread \
         count); call `unsafe {{ daemonize_unchecked(&config) }}` and ensure the \
         process is single-threaded yourself"
    )
}

/// Internal daemonization logic, generic over the Forker trait for testability.
///
/// # Safety
///
/// With a real `Forker` this performs `fork`; the process must be
/// single-threaded at the call, since forking with other threads running is
/// undefined behavior. The checked [`daemonize`] establishes this, and the
/// public [`daemonize_unchecked`] forwards the contract to its caller. (The
/// test `NullForker` does not fork, so test calls are sound.)
#[allow(unsafe_code)]
pub(crate) unsafe fn daemonize_inner(
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

        // SAFETY: daemonize_unchecked() is unsafe and requires the caller to
        // ensure the process is single-threaded. The checked daemonize()
        // verifies this via the kernel thread count before calling
        // daemonize_inner().
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
    // single Result. In foreground mode no fork happened, so errors simply
    // propagate to the caller. In daemon mode the child cannot return to the
    // original caller, so funnel any error through one place: notify the
    // parent and exit. `pipe_wr` stays available so the error path can still
    // signal it.
    match run_post_fork(config, &mut pipe_wr) {
        Ok(ctx) => Ok(ctx),
        Err(e) if foreground => Err(e),
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

    // Step 7: Open and lock lockfile (explicit path, or the pidfile itself
    // unless derivation was opted out)
    let lockfile_path = config.effective_lockfile().map(std::path::PathBuf::as_path);
    let lockfile = match lockfile_path {
        Some(path) => Some(steps::open_and_lock(path)?),
        None => None,
    };

    // Step 8: Write pidfile
    if let Some(ref pidfile_path) = config.pidfile {
        steps::write_pidfile(pidfile_path, lockfile_path.zip(lockfile.as_ref()))?;
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

    // Covers: R45
    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    #[test]
    fn single_threaded_violation_accepts_only_exactly_one() {
        assert!(
            single_threaded_violation("daemonize", 1).is_none(),
            "exactly one thread is the single-threaded case"
        );
        assert!(
            single_threaded_violation("daemonize", 0).is_some(),
            "an anomalous 0 must fail closed, not green-light a fork"
        );
        let msg =
            single_threaded_violation("drop_privileges", 2).expect("2 threads is a violation");
        assert!(
            msg.contains("drop_privileges: 2 threads running (expected 1)"),
            "message should name the caller, count, and expectation, got: {msg}"
        );
    }

    /// Test wrapper: `daemonize_inner` driven by the non-forking `NullForker`.
    ///
    /// `NullForker::fork` returns a configured result without actually forking,
    /// so the `unsafe fn`'s single-threaded contract is vacuously satisfied —
    /// keeping the call sites free of per-test `unsafe` blocks.
    #[allow(unsafe_code)]
    fn run_inner(
        config: &DaemonConfig,
        forker: &mut NullForker,
    ) -> Result<DaemonContext, DaemonizeError> {
        // SAFETY: NullForker does not fork.
        unsafe { daemonize_inner(config, forker) }
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
        let result = run_inner(&config, &mut forker);
        assert!(result.is_ok());
    }

    #[test]
    fn first_fork_parent_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::first_parent();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        assert!(result.is_err()); // exit panics in NullForker
    }

    #[test]
    fn second_fork_parent_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::second_parent();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        assert!(result.is_err());
    }

    // Covers: R57
    #[test]
    fn first_fork_fails_returns_error() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::first_fork_fails();
        let result = run_inner(&config, &mut forker);
        assert!(matches!(result, Err(DaemonizeError::ForkFailed(_))));
    }

    #[test]
    fn setsid_fails_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::setsid_fails();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        assert!(result.is_err());
    }

    // Covers: R58
    #[test]
    fn second_fork_fails_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::second_fork_fails();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        assert!(result.is_err());
    }

    #[test]
    fn exit_panic_contains_code() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::first_parent();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
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

    // Covers: R66, R68
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
        let result = run_inner(&config, &mut forker);
        let ctx = result.expect("foreground daemonize_inner should succeed");
        assert!(ctx.lockfile_fd().is_none());
    }

    // Covers: R131
    #[test]
    fn pidfile_only_holds_derived_lock() {
        run_in_subprocess("tests::pidfile_only_holds_derived_lock_subprocess");
    }

    #[test]
    #[ignore]
    fn pidfile_only_holds_derived_lock_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        let ctx = run_inner(&config, &mut forker).expect("daemonize should succeed");
        let lock_fd = ctx
            .lockfile_fd()
            .expect("a lone pidfile should be flock'd by default");
        // The held lock must be on the pidfile itself, not some other file.
        let lock_stat = nix::sys::stat::fstat(lock_fd).unwrap();
        let pidfile_stat = nix::sys::stat::stat(&pidfile).unwrap();
        assert_eq!(
            (lock_stat.st_dev, lock_stat.st_ino),
            (pidfile_stat.st_dev, pidfile_stat.st_ino),
            "derived lock fd should refer to the pidfile"
        );
        // A second acquisition of the same path must conflict.
        let second = steps::open_and_lock(&pidfile);
        assert!(matches!(second, Err(DaemonizeError::LockConflict(_))));
    }

    // Covers: R134
    #[test]
    fn foreground_lock_conflict_returns_err() {
        run_in_subprocess("tests::foreground_lock_conflict_returns_err_subprocess");
    }

    #[test]
    #[ignore]
    fn foreground_lock_conflict_returns_err_subprocess() {
        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let _held = steps::open_and_lock(&pidfile).expect("first lock should succeed");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        let result = run_inner(&config, &mut forker);
        assert!(
            matches!(result, Err(DaemonizeError::LockConflict(_))),
            "foreground mode should surface setup errors as Err, not exit"
        );
    }

    // Covers: R67
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
        let mut ctx = run_inner(&config, &mut forker).unwrap();
        assert!(ctx.notify_parent().is_ok());
    }

    // Covers: R69
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
        let _ctx = run_inner(&config, &mut forker).unwrap();

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

    // Covers: R120
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
        let ctx = run_inner(&config, &mut forker).unwrap();

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
