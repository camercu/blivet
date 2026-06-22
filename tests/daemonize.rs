//! E2E test for the `daemonize` single-threaded guard (R45).
//!
//! `daemonize` reads the process thread count and panics if more than
//! one thread is running — *before* it forks. So spawning a second thread and
//! calling it is safe: it panics on the check and never daemonizes the test
//! process. Only built on the targets where `daemonize` is the real
//! function (elsewhere it is a deprecated stub).

#![cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use blivet::DaemonConfig;

// Covers: R45
#[test]
fn daemonize_panics_when_not_single_threaded() {
    // Spawn a second thread and keep it parked (alive) so the process has >1
    // thread for the duration of the call.
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel();
    let worker = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            ready_tx.send(()).unwrap();
            while !stop.load(Ordering::Acquire) {
                thread::park();
            }
        })
    };
    ready_rx.recv().unwrap(); // worker is running -> thread count is now >= 2

    // Must panic on the thread-count check, before any fork.
    let config = DaemonConfig::new();
    let result = std::panic::catch_unwind(|| blivet::daemonize(&config));

    // Release the keepalive thread regardless of the outcome.
    stop.store(true, Ordering::Release);
    worker.thread().unpark();
    worker.join().unwrap();

    let payload = result.expect_err("daemonize must panic with >1 thread");
    let msg = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("<non-string panic payload>");
    assert!(
        msg.contains("threads running (expected 1)"),
        "panic should name the thread-count problem, got: {msg:?}"
    );
}
