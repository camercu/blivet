//! Post-fork daemonization steps, one submodule per sub-concept.
//!
//! The sequence in `sequence.rs` runs them in order (steps 4 to 13). Each is
//! a function here rather than inline there because each is independently
//! testable. Callers use `steps::name`; the submodules are an internal
//! grouping.

mod fds;
mod opened;
mod pidfile;
mod process;
mod stdio;
mod undo;

pub(crate) use fds::*;
pub(crate) use opened::*;
pub(crate) use pidfile::*;
pub(crate) use process::*;
pub(crate) use stdio::*;
pub(crate) use undo::*;

/// Test-only failure injection for post-fork syscalls that cannot be made to
/// fail from inside a test process (that would need a missing `/dev/null` or a
/// seccomp filter). A set flag makes its step take the error return so tests
/// can pin that the failure *propagates* out of the sequence rather than being
/// swallowed. Flags are process-global, so setting one asserts the test runs
/// in its own process (`test_support::run_in_subprocess`).
#[cfg(test)]
pub(crate) mod failpoints {
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

    /// One injectable failure. Only [`set`](Self::set) writes it, and that
    /// asserts the test runs in its own process.
    pub(crate) struct Failpoint(AtomicBool);

    impl Failpoint {
        const fn new() -> Self {
            Self(AtomicBool::new(false))
        }

        /// Makes the step fail from now on, in this whole process.
        pub(crate) fn set(&self) {
            crate::test_support::assert_isolated("a failpoint");
            self.0.store(true, Ordering::Relaxed);
        }

        /// True when set — reads with `Relaxed`: set before the sequence runs
        /// and never concurrently.
        pub(crate) fn injected(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
    }

    pub(crate) static DEVNULL_OPEN_FAILS: Failpoint = Failpoint::new();
    pub(crate) static SIGACTION_FAILS: Failpoint = Failpoint::new();
    pub(crate) static SIGPROCMASK_FAILS: Failpoint = Failpoint::new();
    pub(crate) static GETRLIMIT_FAILS: Failpoint = Failpoint::new();
    pub(crate) static FD_LISTING_UNAVAILABLE: Failpoint = Failpoint::new();

    /// Fails `setuid` after `setgid` succeeded, which root cannot make
    /// happen on demand.
    pub(crate) static SETUID_FAILS: Failpoint = Failpoint::new();

    /// Fails the pidfile write *after* the file has been created and
    /// truncated, which is the shape of a real `ENOSPC`/`EIO`. The step-9-13
    /// failpoints cannot reach this: by the time they fire, step 8 has already
    /// returned `Ok`.
    pub(crate) static PIDFILE_WRITE_FAILS: Failpoint = Failpoint::new();

    /// Fails the standalone pidfile `open` itself, before this process has
    /// touched the file — an `EACCES`, say, that root would not hit. What is on
    /// disk then is not this process's to remove.
    pub(crate) static PIDFILE_OPEN_FAILS: Failpoint = Failpoint::new();

    /// Fails step 12's truncation of a stream file, the last thing it does:
    /// every stdio slot has moved by then.
    pub(crate) static STREAM_TRUNCATE_FAILS: Failpoint = Failpoint::new();

    /// Fails the sequence just after it records its Nth undo token, counting
    /// from 1, for the undo walk. 0 fails none.
    static FAIL_AFTER_TOKEN: AtomicUsize = AtomicUsize::new(0);

    /// The error [`FAIL_AFTER_TOKEN`] injects.
    pub(crate) const TOKEN_FAILURE: &str = "undo walk: failed after a token";

    /// Sets [`FAIL_AFTER_TOKEN`], in this whole process; asserts the test
    /// runs in its own process.
    pub(crate) fn fail_after_token(n: usize) {
        crate::test_support::assert_isolated("the undo walk");
        FAIL_AFTER_TOKEN.store(n, Ordering::Relaxed);
    }

    /// Whether the sequence fails having recorded `count` tokens.
    pub(crate) fn token_fails(count: usize) -> bool {
        FAIL_AFTER_TOKEN.load(Ordering::Relaxed) == count
    }

    /// Fails the sequence at the last point a failure can still be undone:
    /// after step 16, before step 17's switch, which no token can reverse.
    pub(crate) static BEFORE_SWITCH_FAILS: Failpoint = Failpoint::new();

    /// The error [`BEFORE_SWITCH_FAILS`] injects.
    pub(crate) const SWITCH_FAILURE: &str = "undo walk: failed before the switch";

    /// Stands in for the fd limit `getrlimit` would report, or 0 to use the
    /// real one.
    ///
    /// A test that wants the brute-force branch wants the branch, not the
    /// machine's `RLIMIT_NOFILE`: closing `3..rlim_cur` is a real loop, and a
    /// host with a large limit makes an otherwise instant test take minutes
    /// (measured: ~0.2s at 1M, and a systemd `LimitNOFILE=infinity` clamps to
    /// `i32::MAX`). Bounding it here keeps the test constant-time on any host.
    static MAX_FD: AtomicI32 = AtomicI32::new(0);

    /// Sets [`MAX_FD`], in this whole process; asserts the test runs in its
    /// own process.
    pub(crate) fn set_max_fd(max_fd: i32) {
        crate::test_support::assert_isolated("the injected fd limit");
        MAX_FD.store(max_fd, Ordering::Relaxed);
    }

    /// The fd limit [`MAX_FD`] stands in with, if a test set one.
    pub(crate) fn injected_max_fd() -> Option<i32> {
        let max_fd = MAX_FD.load(Ordering::Relaxed);
        (max_fd > 0).then_some(max_fd)
    }
}
