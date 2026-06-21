//! Current-process thread count for the single-threaded check in
//! [`daemonize_checked`](crate::daemonize_checked).
//!
//! [`count`] returns the number of threads in this process. Linux reads
//! `/proc/self/status`; macOS and the BSDs query the kernel via
//! [`crate::unsafe_ops::thread_count`] — the syscalls live there because the
//! crate confines all `unsafe` to that module. This module is the single
//! conceptual owner of "how many threads are running"; callers see only
//! [`count`] and never the per-OS mechanism.
//!
//! Defined only on the targets [`daemonize_checked`](crate::daemonize_checked)
//! supports; elsewhere that function is a deprecated stub that never calls in
//! here.

/// Number of threads in the current process, via `/proc/self/status`.
#[cfg(target_os = "linux")]
pub(crate) fn count() -> std::io::Result<usize> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let line = status
        .lines()
        .find(|line| line.starts_with("Threads:"))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no Threads: line"))?;
    line.split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed Threads: line")
        })
}

/// On macOS and the BSDs the count comes from the kernel via the unsafe FFI
/// confined to `unsafe_ops`.
#[cfg(all(
    not(target_os = "linux"),
    any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
pub(crate) use crate::unsafe_ops::thread_count as count;

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
mod tests {
    use super::count;
    use crate::test_support::{is_subprocess, run_in_subprocess};

    /// The thread count backing `daemonize_checked` must reflect the kernel's
    /// real thread count. Runs in an isolated subprocess: the assertion is
    /// relative (the count must *rise* by the number of threads spawned), which
    /// only holds when nothing else changes the count between readings. In the
    /// normal test run libtest's own worker pool fluctuates, so this must be
    /// the only test executing — hence the subprocess.
    #[test]
    fn current_thread_count_tracks_live_threads() {
        run_in_subprocess(
            "thread_count::tests::current_thread_count_tracks_live_threads_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn current_thread_count_tracks_live_threads_subprocess() {
        if !is_subprocess() {
            return;
        }
        use std::sync::mpsc;
        use std::sync::{Arc, Barrier};

        let base = count().expect("thread count should be readable");
        assert!(
            base >= 1,
            "expected at least the calling thread, got {base}"
        );

        const N: usize = 3;
        // N workers + this thread all rendezvous on `release`, so the workers
        // stay alive (blocked) while we re-read the count.
        let release = Arc::new(Barrier::new(N + 1));
        let (started_tx, started_rx) = mpsc::channel();
        let mut handles = Vec::new();
        for _ in 0..N {
            let release = Arc::clone(&release);
            let started_tx = started_tx.clone();
            handles.push(std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                release.wait();
            }));
        }
        for _ in 0..N {
            started_rx.recv().unwrap(); // all N are now running
        }

        let with_threads = count().expect("thread count should be readable");
        assert!(
            with_threads >= base + N,
            "expected >= {} threads with {N} spawned, got {with_threads}",
            base + N
        );

        release.wait();
        for h in handles {
            h.join().unwrap();
        }
    }
}
