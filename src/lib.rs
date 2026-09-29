//! A correct, full-featured Unix daemon library and CLI for Rust.
//!
//! A [blivet] is the "impossible fork" optical illusion, also known as the
//! devil's fork. Daemons are created by forking — and this crate
//! performs the impossible double-fork to do it correctly.
//!
//! This crate provides a library and CLI tool for daemonizing processes on Unix
//! systems. It performs a double-fork, resets signal dispositions and
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
//! config.pidfile("/var/run/foo.pid").chdir("/var/lib/foo");
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
//!   single-threaded for you, so no `unsafe` is needed. It exists wherever the
//!   kernel's own thread count can be read — see
//!   [Platform support](#platform-support). On any other target it is a
//!   `#[deprecated]` stub that never daemonizes — a hard compile error under
//!   `-D warnings` / `#![deny(deprecated)]`; use [`daemonize_unchecked`] there.
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
#![doc = include_str!(concat!(env!("OUT_DIR"), "/entry_point_cfg.md"))]
//!
//! # Platform support
//!
#![doc = include_str!(concat!(env!("OUT_DIR"), "/platform_support.md"))]
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
//! Two `daemonize(1)`-standard defaults bite the unwary: stdout/stderr go to
//! `/dev/null` (a `println!` vanishes), and the working directory becomes `/`
//! (relative paths resolve against `/` and usually fail). Use absolute paths;
//! see [`stdout`](DaemonConfig::stdout) / [`stderr`](DaemonConfig::stderr) /
//! [`chdir`](DaemonConfig::chdir) to change them.
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
//! `Drop` **does not run** when a signal kills the process (how daemons are
//! normally stopped), so the auto-cleanup from
//! [`cleanup_on_drop`](DaemonConfig::cleanup_on_drop) leaves a stale pidfile.
//! Call [`cleanup_on_term_signals`](DaemonContext::cleanup_on_term_signals)
//! once, or run your own shutdown loop and let the context drop — see
//! `examples/echo_server.rs`.
//!
//! # Exit codes
//!
//! [`DaemonizeError::exit_code`] maps each error to a `sysexits.h` code — see
//! its docs for the full table — but `fn main() -> Result<(), E>` ignores it
//! and exits **1**. To surface the codes, call `exit_code()` yourself. To
//! report a failure from your own init code (e.g. a socket bind) with a chosen
//! code, use [`report_error_msg`](DaemonContext::report_error_msg) or the
//! [`DaemonizeError::Application`] variant.
//!
//! # Split-phase design
//!
//! Many daemons need root during startup — bind a privileged port, write a
//! pidfile to `/var/run`, open root-owned logs — but should run unprivileged
//! afterward. Rather than fold privilege dropping into the daemonize call,
//! `daemonize()` returns a [`DaemonContext`] still running as root; you do the
//! privileged work, then call
//! [`drop_privileges()`](DaemonContext::drop_privileges) (which first chowns
//! the pidfile/lockfile/logs to the target user — opt out with
//! [`chown_paths`](DaemonConfig::chown_paths)), then
//! [`notify_parent()`](DaemonContext::notify_parent). Full ordering control:
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
//! // 2. Drop to unprivileged user (chowns pidfile/logs first, while still root)
//! ctx.drop_privileges()?;
//!
//! // 3. Tell the parent we're ready
//! ctx.notify_parent()?;
//!
//! // Daemon continues as "nobody" with the socket still open
//! # Ok(())
//! # }
//! ```

#![deny(unsafe_code)]

// Daemonizing is fork, setsid, signals, and file descriptors — there is no
// non-Unix fallback to take, and without this the failure is a wall of type
// errors from inside `nix` that never says why.
#[cfg(not(unix))]
compile_error!(concat!(
    "blivet daemonizes a Unix process, and ",
    env!("BLIVET_TARGET"),
    " is not Unix."
));

// Every item below is Unix-only, so a non-Unix build of the library stops at
// the `compile_error!` above instead of adding the wall of type errors from
// inside `nix` that it exists to replace. `just check-non-unix` holds that
// property, and holds it for the library alone — the `cfg(test)` modules below
// are not gated, so a non-Unix `--all-targets` build still has more to say.
#[cfg(unix)]
mod config;
#[cfg(unix)]
mod context;
#[cfg(unix)]
mod error;
#[cfg(unix)]
pub(crate) mod forker;
#[cfg(unix)]
mod identity;
#[cfg(unix)]
pub(crate) mod unsafe_ops;

#[cfg(unix)]
mod notify;
#[cfg(unix)]
mod steps;
#[cfg(unix)]
mod thread_count;
#[cfg(unix)]
pub(crate) mod util;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod capability_probes;

#[cfg(test)]
mod doc_sync;

/// Compile-checks every `rust` code block in the README as a doctest, so a
/// stale snippet fails `cargo test` instead of misleading readers. Blocks
/// that daemonize are marked `no_run`; fragments that cannot stand alone are
/// marked `ignore`. Does not affect the docs.rs front page.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme_doctests {}

#[cfg(unix)]
pub use config::DaemonConfig;
#[cfg(unix)]
pub use context::DaemonContext;
#[cfg(unix)]
pub use error::DaemonizeError;

#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::{AsRawFd, OwnedFd};

#[cfg(unix)]
use nix::unistd::ForkResult;

#[cfg(unix)]
use forker::{Forker, RealForker};
#[cfg(unix)]
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
/// never exits the caller's process. A system call that fails with no more
/// specific variant, such as creating the notification pipe with no
/// descriptor free, returns `SystemError`.
#[cfg(unix)]
#[allow(unsafe_code)]
pub unsafe fn daemonize_unchecked(config: &DaemonConfig) -> Result<DaemonContext, DaemonizeError> {
    config.validate()?;
    // SAFETY: this `unsafe fn`'s own contract requires single-threadedness.
    unsafe { daemonize_inner(config, &mut RealForker) }
}

// The `cfg`s below gate on `blivet_thread_count`, the capability alias
// `build.rs` sets for targets where the live thread count can be read. The
// count itself lives in the `thread_count` module.

/// Returns the panic message if `count` is not exactly one thread, else `None`.
///
/// The checked entry points require *exactly* one thread (R45): forking — or
/// calling `setenv` during `drop_privileges` — in a multi-threaded process is
/// unsound. Any count other than 1 is a violation — including an anomalous `0`,
/// which a healthy process can never report and so signals an unreliable
/// thread-count query. Failing closed keeps the safety guard from
/// green-lighting on a count it cannot trust. `caller` names the operation in
/// the panic message.
#[cfg(unix)]
#[cfg(blivet_thread_count)]
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
#[cfg(unix)]
#[cfg(blivet_thread_count)]
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
/// Available wherever the kernel's own thread count can be read — the crate
/// front page lists those platforms and where each reads its count from. On any
/// other target this is a `#[deprecated]` stub that never daemonizes — calling
/// it warns with guidance (and is a hard compile error under `-D warnings` /
/// `#![deny(deprecated)]`), and panics if invoked anyway; call
/// [`daemonize_unchecked`] yourself there inside an `unsafe` block.
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
#[cfg(unix)]
#[cfg(blivet_thread_count)]
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
#[cfg(unix)]
#[cfg(not(blivet_thread_count))]
#[cfg_attr(test, mutants::skip)]
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
#[cfg(unix)]
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
        // Made in the caller's process, before any fork, so a failure
        // returns to the caller.
        let (pipe_rd, pipe_wr) = forker.create_notification_pipe()?;
        let pipe_wr = NotifyPipe::new(pipe_wr);

        // SAFETY: daemonize_unchecked() is unsafe and requires the caller to
        // ensure the process is single-threaded. The checked daemonize()
        // verifies this via the kernel thread count before calling
        // daemonize_inner().
        let first_fork = unsafe { forker.fork() };
        let first_fork = match first_fork {
            Ok(result) => result,
            Err(e) => {
                // No child exists yet; the error returns to the caller
                // directly, so close the write end silently rather than let
                // the NotifyPipe Drop safety net write into a pipe whose only
                // reader is this same process.
                pipe_wr.close();
                return Err(e);
            }
        };
        match first_fork {
            ForkResult::Parent { .. } => {
                // Parent: close the write end *silently* (it only reads) then
                // read and exit. A plain drop would trip the NotifyPipe Drop
                // safety net and make the parent read its own failure bytes.
                pipe_wr.close();
                parent_pipe_reader(pipe_rd, forker);
            }
            ForkResult::Child => {
                // Child: close read end, continue
                drop(pipe_rd);
            }
        }

        // Step 2: setsid
        if let Err(e) = forker.setsid() {
            pipe_wr.signal_error(&e);
            forker.exit(e.exit_code() as i32);
        }

        // Step 3: Second fork
        // SAFETY: same as above — single-threaded post-fork child.
        match unsafe { forker.fork() } {
            Ok(ForkResult::Parent { .. }) => {
                // Intermediate child exits silently: the grandchild daemon is
                // the sole writer, so close the write-end copy without tripping
                // the NotifyPipe Drop safety net.
                pipe_wr.close();
                forker.exit(0);
            }
            Ok(ForkResult::Child) => {
                // Grandchild continues
            }
            Err(e) => {
                pipe_wr.signal_error(&e);
                forker.exit(e.exit_code() as i32);
            }
        }

        Some(pipe_wr)
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
            if let Some(pipe) = pipe_wr.take() {
                pipe.signal_error(&e);
            }
            forker.exit(e.exit_code() as i32);
        }
    }
}

/// Removes the pidfile this process wrote if the sequence never reaches the end.
///
/// Everything from the second half of step 8 through step 13 runs after this
/// process has taken hold of the pidfile, and any of it can fail. In daemon mode the abort path signals the parent and `_exit`s, and in
/// foreground mode the error returns to the caller; neither builds a
/// [`DaemonContext`], so no drop-time cleanup ever runs. Without this the file
/// is left naming a process that never started — what `kill $(cat …)`, a status
/// script, or systemd's `PIDFile=` reads next, and after PID reuse some other
/// process entirely.
///
/// Armed once [`steps::open_pidfile`] returns, which is when the file becomes
/// this process's — not before, so a lock conflict or a refused `open` cannot
/// remove a file that belongs to someone else, and not after the write, so a
/// write that fails half way does not leave an empty pidfile behind. Gated on
/// [`cleanup_on_drop`](DaemonConfig::cleanup_on_drop), matching the drop-time
/// cleanup that [`DaemonContext::report_error`] replicates for the same reason.
#[cfg(unix)]
struct PidfileOnAbort<'a> {
    path: Option<&'a std::path::Path>,
}

#[cfg(unix)]
impl PidfileOnAbort<'_> {
    /// The sequence reached the end; the [`DaemonContext`] owns the pidfile now.
    fn disarm(&mut self) {
        self.path = None;
    }
}

#[cfg(unix)]
impl Drop for PidfileOnAbort<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Steps 4–14: apply the configuration in the final daemon process.
///
/// Forker-free and fallible: every step returns its error rather than touching
/// the notification pipe, leaving the single error-to-parent seam in
/// [`daemonize_inner`]. On success the notification pipe write end is moved into
/// the returned [`DaemonContext`]; `pipe_wr` is left `None`.
#[cfg(unix)]
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
    steps::redirect_to_devnull(!config.foreground)?;

    // Step 7: Open and lock lockfile (explicit path, or the pidfile itself
    // unless derivation was opted out)
    let lockfile_path = config.effective_lockfile().map(std::path::PathBuf::as_path);
    let lockfile = match lockfile_path {
        Some(path) => Some(steps::open_and_lock(path)?),
        None => None,
    };

    // Step 8: Write pidfile. The debt is owed from the moment this process
    // owns the file on disk — once `open_pidfile` returns, having locked it at
    // step 7 or created and truncated it just now — not from the moment the
    // write succeeds: a write that fails half way leaves an empty file naming
    // no process at all. Nor any earlier: an `open_pidfile` that fails, or a
    // lock conflict at step 7, leaves a file that belongs to someone else.
    // Arm between the two halves, disarm at step 14 — see `PidfileOnAbort`.
    let mut pidfile_on_abort = PidfileOnAbort { path: None };
    // The files the sequence now holds, which step 12 must not redirect into.
    // The pidfile goes first: a derived lockfile is the same file, and the
    // user configured it as the pidfile.
    let mut owned = Vec::new();
    if let Some(ref pidfile_path) = config.pidfile {
        let pidfile = steps::open_pidfile(pidfile_path, lockfile_path.zip(lockfile.as_ref()))?;
        if config.cleanup_on_drop {
            pidfile_on_abort.path = Some(pidfile_path);
        }
        owned.push(("pidfile", pidfile.write_pid()?));
    }
    if let Some(ref lockfile) = lockfile {
        let id = steps::file_id(&**lockfile)
            .map_err(|e| DaemonizeError::LockfileError(format!("fstat: {e}")))?;
        owned.push(("lockfile", id));
    }

    // Step 9: Reset signal dispositions
    unsafe_ops::reset_signal_dispositions()?;

    // Step 10: Clear signal mask
    steps::clear_signal_mask()?;

    // Step 11: Set environment variables
    steps::set_env_vars(&config.env);

    // Step 12: Redirect stdout/stderr to configured files. Only a foreground
    // caller sees the slots after a failure, so only it pays for saving them.
    if config.stdout.is_some() || config.stderr.is_some() {
        steps::redirect_output(
            config.stdout.as_deref(),
            config.stderr.as_deref(),
            config.append,
            &owned,
            if config.foreground {
                steps::Rollback::PutBack
            } else {
                steps::Rollback::Skip
            },
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
        steps::close_inherited_fds(&skip_fds)?;
    }

    // Step 14: Return DaemonContext (clones the config-derived fields it needs).
    // The context owns the pidfile from here, so the abort guard stands down.
    pidfile_on_abort.disarm();
    Ok(DaemonContext::new(config, lockfile, pipe_wr.take()))
}

/// Parent-side pipe reader. Reads from the pipe and exits accordingly.
#[cfg(unix)]
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
    #[cfg(blivet_thread_count)]
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
        // The fork arms are under test here; step 13 has its own tests.
        config.close_fds(false);
        let mut forker = NullForker::both_child();
        let result = run_inner(&config, &mut forker);
        assert!(result.is_ok());
    }

    // Covers: R137
    #[test]
    fn first_fork_parent_closes_pipe_silently() {
        // The parent's write-end copy must be *closed*, not dropped: NotifyPipe's
        // Drop safety net writes failure bytes, and a parent that drops would
        // read its own bytes and report a failure for a healthy start. With the
        // write end closed silently, the reader sees EOF -> success -> exit(0).
        let config = DaemonConfig::new();
        let mut forker = NullForker::first_parent();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        let panic_msg = result
            .expect_err("parent exits after reading the pipe")
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(
            panic_msg.contains("NullForker::exit(0)"),
            "parent must read EOF = success from its own silently-closed write \
             end, got: {panic_msg}"
        );
    }

    // Covers: R137
    #[test]
    fn second_fork_intermediate_closes_pipe_silently() {
        // The intermediate child's write-end copy must also close without
        // writing: the grandchild daemon is the sole writer on the pipe.
        let config = DaemonConfig::new();
        let mut forker = NullForker::second_parent();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        let panic_msg = result
            .expect_err("intermediate child exits")
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(panic_msg.contains("NullForker::exit(0)"), "{panic_msg}");

        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader for the test");
        let mut buf = Vec::new();
        std::fs::File::from(rd).read_to_end(&mut buf).unwrap();
        assert_eq!(
            buf,
            Vec::<u8>::new(),
            "intermediate child must close the pipe without writing; the \
             daemon is the sole writer"
        );
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
    fn first_fork_failure_closes_pipe_silently() {
        // The error goes back to the caller directly — nothing should reach
        // the pipe, whose only reader is this same process. A plain drop would
        // fire the NotifyPipe Drop safety net into it.
        let config = DaemonConfig::new();
        let mut forker = NullForker::first_fork_fails();
        let result = run_inner(&config, &mut forker);
        assert!(matches!(result, Err(DaemonizeError::ForkFailed(_))));

        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader");
        let mut buf = Vec::new();
        std::fs::File::from(rd).read_to_end(&mut buf).unwrap();
        assert_eq!(
            buf,
            Vec::<u8>::new(),
            "a pre-fork failure must leave the pipe unwritten"
        );
    }

    #[test]
    fn setsid_failure_reports_to_the_parent_and_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::setsid_fails();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        let panic_msg = result
            .expect_err("the child exits")
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(panic_msg.contains("NullForker::exit(71)"), "{panic_msg}");

        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader");
        let mut buf = Vec::new();
        std::fs::File::from(rd).read_to_end(&mut buf).unwrap();
        assert_eq!(
            buf,
            notify::error_bytes(&DaemonizeError::SetsidFailed("test".into()))
        );
    }

    // Covers: R58
    #[test]
    fn second_fork_failure_reports_to_the_parent_and_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::second_fork_fails();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        let panic_msg = result
            .expect_err("the child exits")
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(panic_msg.contains("NullForker::exit(71)"), "{panic_msg}");

        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader");
        let mut buf = Vec::new();
        std::fs::File::from(rd).read_to_end(&mut buf).unwrap();
        assert_eq!(
            buf,
            notify::error_bytes(&DaemonizeError::ForkFailed("second fork".into()))
        );
    }

    // A post-fork step failing in daemon mode must funnel through the
    // notify-parent-and-exit arm (lib.rs), not return the error to the caller —
    // the child has no caller to return to. Distinct from a fork failure, which
    // returns before run_post_fork. Kills the mutant that swaps the foreground
    // and daemon match arms: with the arms swapped, this would return Err
    // instead of exiting.
    #[test]
    #[serial_test::serial]
    fn daemon_post_fork_failure_exits_not_returns() {
        // Step 4 sets the process umask from the config before chdir fails;
        // the guard restores the harness's umask even if an assertion panics.
        let _umask = test_support::UmaskGuard::set(nix::sys::stat::Mode::empty());
        // Daemon mode (foreground stays false); chdir to a nonexistent path
        // fails at step 5, before any stdio redirection touches this process.
        let mut config = DaemonConfig::new();
        config.chdir("/nonexistent_daemonize_postfork_failure");
        let mut forker = NullForker::both_child();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));

        let panic_msg = result
            .expect_err("daemon-mode post-fork failure must exit (panic), not return")
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(
            panic_msg.contains("NullForker::exit(71)"),
            "must exit with ChdirFailed's EX_OSERR code 71, got: {panic_msg}"
        );
    }

    // Covers: R66, R68, R121
    #[test]
    fn foreground_mode_skips_fork() {
        run_in_subprocess("tests::foreground_mode_skips_fork_subprocess");
    }

    #[test]
    #[ignore]
    fn foreground_mode_skips_fork_subprocess() {
        use nix::sys::stat::fstat;

        if !is_subprocess() {
            return;
        }
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false);
        // No fork is scripted, so a fork panics; setsid is scripted to fail,
        // so a setsid surfaces as an error. Either would fail the run.
        let mut forker = NullForker::new(
            vec![],
            Err(DaemonizeError::SetsidFailed(
                "foreground must not setsid".into(),
            )),
        );
        let stdout_before = fstat(std::io::stdout()).unwrap();

        let result = run_inner(&config, &mut forker);
        let ctx = result.expect("foreground daemonize_inner should succeed");
        assert!(ctx.lockfile_fd().is_none());

        // Step 6 leaves stdout inherited in foreground mode, rather than
        // pointing it at /dev/null.
        let stdout_after = fstat(std::io::stdout()).unwrap();
        assert_eq!(
            (stdout_before.st_dev, stdout_before.st_ino),
            (stdout_after.st_dev, stdout_after.st_ino),
            "foreground mode redirected stdout"
        );
    }

    // Covers: R148
    #[test]
    fn notification_pipe_failure_returns_err_before_any_fork() {
        let config = DaemonConfig::new();
        // No fork is scripted, so a fork panics.
        let mut forker = NullForker::new(vec![], Ok(())).with_failing_pipe();

        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));

        let result = result.expect("a pipe failure must return, not fork or exit");
        assert!(
            matches!(result, Err(DaemonizeError::SystemError(_))),
            "{result:?}"
        );
    }

    // Covers: R147
    #[test]
    fn foreground_redirect_that_fails_puts_stdout_back() {
        run_in_subprocess("tests::foreground_redirect_that_fails_puts_stdout_back_subprocess");
    }

    /// A foreground caller sees its stdio after a failed start, so step 12
    /// must save and restore fd 1 there.
    #[test]
    #[ignore]
    fn foreground_redirect_that_fails_puts_stdout_back_subprocess() {
        use nix::sys::stat::fstat;

        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout(dir.path().join("app.log"));
        let mut forker = NullForker::new(vec![], Ok(()));
        let before = fstat(std::io::stdout()).unwrap();
        steps::failpoints::STREAM_TRUNCATE_FAILS.store(true, std::sync::atomic::Ordering::Relaxed);

        let result = run_inner(&config, &mut forker);

        assert!(
            matches!(result, Err(DaemonizeError::OutputFileError(_))),
            "{result:?}"
        );
        let after = fstat(std::io::stdout()).unwrap();
        assert_eq!(
            (before.st_dev, before.st_ino),
            (after.st_dev, after.st_ino),
            "a failed foreground step 12 left fd 1 moved"
        );
    }

    // Covers: R147
    #[test]
    fn daemon_redirect_starts_with_no_descriptor_free_to_save_stdout() {
        run_in_subprocess(
            "tests::daemon_redirect_starts_with_no_descriptor_free_to_save_stdout_subprocess",
        );
    }

    /// Step 12 saves fd 1 only where a failure could be seen: a daemon child
    /// that inherited descriptors up to its limit still gets past it.
    #[test]
    #[ignore]
    fn daemon_redirect_starts_with_no_descriptor_free_to_save_stdout_subprocess() {
        use nix::sys::stat::{fstat, stat};

        if !is_subprocess() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let mut config = DaemonConfig::new();
        config.close_fds(false).stdout(&log);
        let mut forker = NullForker::both_child();

        // Fill every descriptor under a small limit, then free two, which the
        // notification pipe's ends take. The child drops its read end, freeing
        // one; step 6 borrows it for /dev/null and gives it back, and the
        // stdout file then takes it, so saving fd 1 would find none.
        let held = crate::test_support::fill_fd_table(2);

        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        // Exactly one descriptor is free now: the stdout file's own, closed
        // once it was on fd 1. A second would have let step 12 save fd 1, and
        // the test would prove nothing.
        let clone = || std::os::fd::AsFd::as_fd(&std::io::stdin()).try_clone_to_owned();
        let spare = (clone(), clone());
        drop(held);

        let ctx = result
            .expect("a daemon with no free descriptor must start, not exit")
            .unwrap();
        drop(ctx);
        assert!(
            spare.0.is_ok() && spare.1.is_err(),
            "not exactly one descriptor free: {spare:?}"
        );
        let on_fd1 = fstat(std::io::stdout()).unwrap();
        let file = stat(&log).unwrap();
        assert_eq!(
            (on_fd1.st_dev, on_fd1.st_ino),
            (file.st_dev, file.st_ino),
            "fd 1 is not the stdout file"
        );
    }

    // The three SystemError-producing steps cannot be made to fail from inside
    // a test process (that needs a missing /dev/null or a seccomp filter), so
    // each propagation test flips a steps::failpoints flag and asserts the
    // error surfaces from daemonize_inner instead of being swallowed — killing
    // mutants like `let _ = steps::clear_signal_mask();` in run_post_fork.
    // Subprocess-isolated: the flags are process-global, and run_post_fork
    // mutates umask/cwd/stdin for real on the way to the failing step.

    // Covers: R95
    #[test]
    fn devnull_failure_propagates() {
        run_in_subprocess("tests::devnull_failure_propagates_subprocess");
    }

    #[test]
    #[ignore]
    fn devnull_failure_propagates_subprocess() {
        if !is_subprocess() {
            return;
        }
        steps::failpoints::DEVNULL_OPEN_FAILS.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false);
        let mut forker = NullForker::new(vec![], Ok(()));
        match run_inner(&config, &mut forker) {
            Err(DaemonizeError::SystemError(msg)) => {
                assert!(
                    msg.contains("/dev/null"),
                    "message names the syscall: {msg}"
                );
            }
            other => panic!("expected SystemError to propagate, got {other:?}"),
        }
    }

    // Covers: R145
    #[test]
    fn pidfile_write_failure_leaves_no_pidfile() {
        run_in_subprocess("tests::pidfile_write_failure_leaves_no_pidfile_subprocess");
    }

    #[test]
    #[ignore]
    fn pidfile_write_failure_leaves_no_pidfile_subprocess() {
        if !is_subprocess() {
            return;
        }
        steps::failpoints::PIDFILE_WRITE_FAILS.store(true, std::sync::atomic::Ordering::Relaxed);
        let dir = crate::test_support::tmp_dir();
        let pidfile = dir.join(format!("pidwrite-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pidfile);
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        match run_inner(&config, &mut forker) {
            Err(DaemonizeError::PidfileError(msg)) => {
                assert!(msg.contains("write"), "message names the operation: {msg}");
            }
            other => panic!("expected PidfileError to propagate, got {other:?}"),
        }
        assert!(
            !pidfile.exists(),
            "step 8 failed after creating the pidfile and left it behind, \
             empty, naming no process at all"
        );
    }

    // Covers: R145
    #[test]
    fn pidfile_open_failure_leaves_the_existing_file_alone() {
        run_in_subprocess("tests::pidfile_open_failure_leaves_the_existing_file_alone_subprocess");
    }

    #[test]
    #[ignore]
    fn pidfile_open_failure_leaves_the_existing_file_alone_subprocess() {
        if !is_subprocess() {
            return;
        }
        steps::failpoints::PIDFILE_OPEN_FAILS.store(true, std::sync::atomic::Ordering::Relaxed);
        let dir = crate::test_support::tmp_dir();
        let pidfile = dir.join(format!("pidopen-{}.pid", std::process::id()));
        std::fs::write(&pidfile, "4242\n").unwrap();
        let mut config = DaemonConfig::new();
        // No lockfile, so step 7 does not open the pidfile and step 8's own
        // open is the first time this process would touch it.
        config
            .foreground(true)
            .close_fds(false)
            .pidfile(&pidfile)
            .no_lockfile();
        let mut forker = NullForker::new(vec![], Ok(()));
        let result = run_inner(&config, &mut forker);
        let left = std::fs::read_to_string(&pidfile);
        let _ = std::fs::remove_file(&pidfile);

        assert!(
            matches!(&result, Err(DaemonizeError::PidfileError(m)) if m.contains("open")),
            "expected the open failure to propagate, got {result:?}"
        );
        assert_eq!(
            left.ok().as_deref(),
            Some("4242\n"),
            "an open that failed removed a pidfile this process never opened"
        );
    }

    // Covers: R134, R145
    #[test]
    fn sigaction_failure_propagates() {
        run_in_subprocess("tests::sigaction_failure_propagates_subprocess");
    }

    #[test]
    #[ignore]
    fn sigaction_failure_propagates_subprocess() {
        if !is_subprocess() {
            return;
        }
        steps::failpoints::SIGACTION_FAILS.store(true, std::sync::atomic::Ordering::Relaxed);
        let dir = crate::test_support::tmp_dir();
        let pidfile = dir.join(format!("sigaction-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pidfile);
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        match run_inner(&config, &mut forker) {
            Err(DaemonizeError::SystemError(msg)) => {
                assert!(
                    msg.contains("sigaction"),
                    "message names the syscall: {msg}"
                );
            }
            other => panic!("expected SystemError to propagate, got {other:?}"),
        }
        assert!(
            !pidfile.exists(),
            "a sequence that aborted after step 8 left its pidfile behind, \
             naming a process that never started"
        );
    }

    // Covers: R134, R145
    #[test]
    fn sigprocmask_failure_propagates() {
        run_in_subprocess("tests::sigprocmask_failure_propagates_subprocess");
    }

    #[test]
    #[ignore]
    fn sigprocmask_failure_propagates_subprocess() {
        if !is_subprocess() {
            return;
        }
        steps::failpoints::SIGPROCMASK_FAILS.store(true, std::sync::atomic::Ordering::Relaxed);
        let dir = crate::test_support::tmp_dir();
        let pidfile = dir.join(format!("sigprocmask-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pidfile);
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        match run_inner(&config, &mut forker) {
            Err(DaemonizeError::SystemError(msg)) => {
                assert!(
                    msg.contains("sigprocmask"),
                    "message names the syscall: {msg}"
                );
            }
            other => panic!("expected SystemError to propagate, got {other:?}"),
        }
        assert!(
            !pidfile.exists(),
            "a sequence that aborted after step 8 left its pidfile behind, \
             naming a process that never started"
        );
    }

    // Covers: R105, R145
    #[test]
    fn getrlimit_failure_propagates() {
        run_in_subprocess("tests::getrlimit_failure_propagates_subprocess");
    }

    #[test]
    #[ignore]
    fn getrlimit_failure_propagates_subprocess() {
        if !is_subprocess() {
            return;
        }
        // Force the brute-force fallback (the fd listing normally short-circuits
        // it on Linux/macOS), then fail its getrlimit. get_max_fd errors before
        // any fd is closed, so the subprocess's descriptors survive.
        steps::failpoints::FD_LISTING_UNAVAILABLE.store(true, std::sync::atomic::Ordering::Relaxed);
        steps::failpoints::GETRLIMIT_FAILS.store(true, std::sync::atomic::Ordering::Relaxed);
        let dir = crate::test_support::tmp_dir();
        let pidfile = dir.join(format!("getrlimit-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pidfile);
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(true).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        match run_inner(&config, &mut forker) {
            Err(DaemonizeError::SystemError(msg)) => {
                assert!(
                    msg.contains("getrlimit"),
                    "message names the syscall: {msg}"
                );
            }
            other => panic!("expected SystemError to propagate, got {other:?}"),
        }
        assert!(
            !pidfile.exists(),
            "a sequence that aborted after step 8 left its pidfile behind, \
             naming a process that never started"
        );
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
        assert!(matches!(second, Err(DaemonizeError::LockConflict { .. })));
    }

    // Covers: R134, R145
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
        // The running daemon's own pidfile, which this attempt must not touch.
        std::fs::write(&pidfile, "999999\n").expect("the incumbent's pidfile");

        let result = run_inner(&config, &mut forker);
        assert!(
            matches!(result, Err(DaemonizeError::LockConflict { .. })),
            "foreground mode should surface setup errors as Err, not exit"
        );
        // The abort guard removes the pidfile a failed sequence wrote. This
        // sequence failed at step 7, before step 8 wrote anything, so the file
        // on disk belongs to the daemon already running — deleting it would
        // strand whoever is supervising it.
        assert_eq!(
            std::fs::read_to_string(&pidfile).expect("the incumbent's pidfile survives"),
            "999999\n",
            "a lock conflict removed the running daemon's pidfile"
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

    // Covers: R69, R122
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
