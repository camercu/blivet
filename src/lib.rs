//! A correct, full-featured Unix daemon library and CLI for Rust.
//!
//! A [blivet] is the "impossible fork" optical illusion, also known as the
//! devil's fork. Daemons are created by forking — and this crate
//! performs the impossible double-fork to do it correctly.
//!
//! This crate provides a library and CLI tool for daemonizing processes on Unix
//! systems. It performs a double-fork, resets signal dispositions and
//! mask, and uses a notification pipe so the parent can wait for daemon
//! readiness. When a user or group is configured, the daemon switches to it
//! before `daemonize()` returns; work that needs the original privileges runs
//! inside the call, through [`daemonize_with`].
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
//! mutexes held by other threads stay locked forever in the child. Switching
//! user is also thread-unsafe, because it calls `setenv`
//! (`USER`/`HOME`/`LOGNAME`). Both happen inside the daemonize call, so the
//! call is the single-threaded window:
//!
//! ```text
//! daemonize_with(|| init)  <- fork, init, switch
//! │ threads, async, accept loops OK
//! notify_parent()          <- writes readiness
//! ```
//!
//! [`daemonize`] and [`daemonize_with`] check for you and panic if violated:
//! at the call, and after `init` when a user switch follows. An `init` may
//! start threads only if it joins them before it returns.
//! [`daemonize_unchecked`] and [`daemonize_with_unchecked`] are the `unsafe`
//! opt-outs. [`notify_parent`](DaemonContext::notify_parent) itself is
//! thread-safe.
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
//! code, use [`WithExitCode`] or [`DaemonizeError::application`] in `init`, or call
//! [`report_error_msg`](DaemonContext::report_error_msg) after the call.
//!
//! # Privileged init
//!
//! Many daemons need root during startup, to bind a privileged port or open
//! root-owned files, but should run unprivileged afterward. Configure the
//! [`user`](DaemonConfig::user) and [`group`](DaemonConfig::group) and do that
//! work in [`daemonize_with`]'s `init`. It runs in the daemon after the
//! pidfile lock, so a second instance fails before its `init` runs. Then
//! blivet chowns the pidfile, lockfile, and logs to the target user (opt out
//! with [`chown_paths`](DaemonConfig::chown_paths)) and switches to it before
//! returning. See ADR 0004 for why `init` runs inside the call.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use blivet::{daemonize_with, DaemonConfig, WithExitCode};
//!
//! let mut config = DaemonConfig::new();
//! config.pidfile("/var/run/foo.pid").user("nobody").group("nogroup");
//!
//! let (mut ctx, listener) = daemonize_with(&config, || {
//!     // Still root, holding the lock.
//!     std::net::TcpListener::bind("0.0.0.0:80").with_exit_code(71)
//! })?;
//!
//! // Running as "nobody" with the socket still open.
//! ctx.notify_parent()?;
//! # drop(listener);
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
mod signals;
#[cfg(unix)]
pub(crate) mod sys;

#[cfg(unix)]
mod file_identity;
#[cfg(unix)]
mod notify;
#[cfg(unix)]
mod sequence;
#[cfg(unix)]
mod steps;
#[cfg(unix)]
mod thread_count;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod capability_probes;

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
pub use error::{DaemonizeError, WithExitCode};

#[cfg(unix)]
use forker::RealForker;

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
    unsafe { sequence::daemonize_inner(config, &mut RealForker) }
}

/// [`daemonize_unchecked`], running privileged `init` inside the sequence.
///
/// `init` runs in the daemon process after the lock is taken and inherited
/// descriptors are closed, while the process still has the privileges it
/// started with. Its value comes back beside the [`DaemonContext`]. See
/// [`daemonize_with`], the checked form.
///
/// # Safety
///
/// As [`daemonize_unchecked`], and `init` must not leave a thread running.
///
/// # Errors
///
/// As [`daemonize_unchecked`]. An error from `init` is the sequence's error:
/// returned in foreground mode, sent to the launcher with its exit code in
/// daemon mode. Either way the pidfile the sequence wrote is removed, unless
/// [`cleanup_on_drop`](DaemonConfig::cleanup_on_drop) is `false`.
#[cfg(unix)]
#[allow(unsafe_code)]
pub unsafe fn daemonize_with_unchecked<T>(
    config: &DaemonConfig,
    init: impl FnOnce() -> Result<T, DaemonizeError>,
) -> Result<(DaemonContext, T), DaemonizeError> {
    config.validate()?;
    // SAFETY: this `unsafe fn`'s own contract requires single-threadedness.
    unsafe { sequence::daemonize_inner_with(config, &mut RealForker, init) }
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
    thread_count::assert_single_threaded("daemonize");
    #[allow(unsafe_code)]
    // SAFETY: verified single-threaded above, and there is no init to start a
    // thread before the user switch.
    unsafe {
        daemonize_unchecked(config)
    }
}

/// Daemonize, running privileged `init` in the daemon before it returns.
///
/// Use it for work that needs the privileges the process started with, or
/// that must happen only once per running daemon: binding a port below 1024,
/// opening root-only files, setting rlimits. `init` runs in the daemon process after
/// the pidfile lock is taken, so a second instance fails with
/// [`LockConflict`](DaemonizeError::LockConflict) before its `init` runs. It
/// runs after inherited descriptors are closed, so what it opens stays open.
/// Whatever it returns comes back beside the [`DaemonContext`].
///
/// A `chroot` in `init` is not supported (ADR 0005). The chown and the
/// supplementary groups still come out right, but the `chdir` into the jail
/// is yours, and pidfile cleanup removes by path, which then names a file
/// inside the jail.
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use blivet::{daemonize_with, DaemonConfig, WithExitCode};
///
/// let mut config = DaemonConfig::new();
/// config.pidfile("/var/run/myapp.pid");
///
/// // An init error reaches the launcher with its exit code: 71 here.
/// let (mut ctx, listener) = daemonize_with(&config, || {
///     std::net::TcpListener::bind("0.0.0.0:80").with_exit_code(71)
/// })?;
/// ctx.notify_parent()?;
/// # drop(listener);
/// # Ok(())
/// # }
/// ```
///
/// # Platform support
///
/// As [`daemonize`].
///
/// # Errors
///
/// As [`daemonize_with_unchecked`].
///
/// # Panics
///
/// As [`daemonize`].
#[cfg(unix)]
#[cfg(blivet_thread_count)]
pub fn daemonize_with<T>(
    config: &DaemonConfig,
    init: impl FnOnce() -> Result<T, DaemonizeError>,
) -> Result<(DaemonContext, T), DaemonizeError> {
    thread_count::assert_single_threaded("daemonize_with");
    #[allow(unsafe_code)]
    // SAFETY: verified single-threaded above, and `checked_init` verifies it
    // again after init, before the user switch's `setenv`.
    unsafe {
        daemonize_with_unchecked(config, thread_count::checked_init(config, init))
    }
}

/// Stub for targets where the thread count cannot be queried. See the
/// [`daemonize`] stub.
///
/// # Panics
///
/// Always panics: the operation is unsupported on this target.
#[cfg(unix)]
#[cfg(not(blivet_thread_count))]
#[cfg_attr(test, mutants::skip)]
#[deprecated(
    note = "daemonize_with cannot verify the thread count on this target. \
            Call `unsafe { daemonize_with_unchecked(&config, init) }` and ensure the \
            process is single-threaded yourself."
)]
pub fn daemonize_with<T>(
    _config: &DaemonConfig,
    _init: impl FnOnce() -> Result<T, DaemonizeError>,
) -> Result<(DaemonContext, T), DaemonizeError> {
    panic!(
        "daemonize_with is unsupported on this target (cannot query the thread \
         count); call `unsafe {{ daemonize_with_unchecked(&config, init) }}` and \
         ensure the process is single-threaded yourself"
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    // Covers: R46, R47, R48
    #[test]
    fn send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DaemonConfig>();
        assert_send_sync::<DaemonContext>();
        assert_send_sync::<DaemonizeError>();
    }
}
