//! The single-threaded checks the checked entry points make, and the thread
//! count behind them.
//!
//! [`ThreadCount`] reads the number of threads in this process: from
//! `/proc/self/status` where `/proc` has it, otherwise from the kernel's
//! per-OS query below (the FFI `unsafe` this module allows).
//! [`assert_single_threaded`] is the check at the entry points (R45), and
//! [`checked_init`] the one after a `daemonize_with` init when a user switch
//! follows (R126).
//!
//! Defined only where the count is available at all (`blivet_thread_count`);
//! elsewhere the checked entry points are deprecated stubs that never call in
//! here.

#![allow(unsafe_code)]

#[cfg(blivet_thread_count)]
use crate::{DaemonConfig, DaemonizeError};

/// This process's thread count, opened once and read as often as needed.
///
/// Where the count comes from `/proc/self/status`, the file is opened here and
/// re-read through its descriptor, so a reading taken after the process
/// changed root, into a jail with no `/proc`, still works (ADR 0005). The
/// kernel queries elsewhere need nothing opened.
#[cfg(blivet_thread_count)]
pub(crate) struct ThreadCount {
    #[cfg(blivet_thread_count_procfs)]
    status: std::fs::File,
}

#[cfg(blivet_thread_count)]
impl ThreadCount {
    pub(crate) fn open() -> std::io::Result<Self> {
        Ok(Self {
            #[cfg(blivet_thread_count_procfs)]
            status: std::fs::File::open("/proc/self/status")?,
        })
    }

    /// The count now.
    #[cfg(blivet_thread_count_procfs)]
    pub(crate) fn read(&self) -> std::io::Result<usize> {
        use std::io::{Read, Seek};
        let mut file = &self.status;
        file.seek(std::io::SeekFrom::Start(0))?;
        let mut status = String::new();
        file.read_to_string(&mut status)?;
        parse_threads_line(&status)
    }

    /// The count now.
    #[cfg(not(blivet_thread_count_procfs))]
    pub(crate) fn read(&self) -> std::io::Result<usize> {
        count()
    }
}

/// The count on the `Threads:` line of a `/proc/<pid>/status` file.
#[cfg(blivet_thread_count_procfs)]
fn parse_threads_line(status: &str) -> std::io::Result<usize> {
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

/// Number of threads in the current process, for the single-threaded check in
/// [`daemonize`](crate::daemonize) on platforms without
/// `/proc/self/status` (Linux uses `/proc`).
///
/// Each target reads the kernel's own thread count for this process:
/// - **macOS:** `proc_pidinfo(PROC_PIDTASKINFO)` → `pti_threadnum`.
/// - **FreeBSD:** `sysctl(KERN_PROC_PID)` → `kinfo_proc.ki_numthreads`.
/// - **NetBSD:** `sysctl(KERN_PROC2/KERN_PROC_PID)` → `kinfo_proc2.p_nlwps`.
/// - **OpenBSD:** `sysctl(KERN_PROC_PID | KERN_PROC_SHOW_THREADS)`; the kernel
///   returns one record per thread, so the count is `oldlen / record_size`.
// The per-OS kernel queries below carry `mutants::skip`. cargo-mutants
// mutates a `cfg`-gated function even where it is not compiled, so on the
// Linux mutation run each one "survives" whenever a diff touches it: no test
// there can see it. Each is tested on its own OS instead, by the capability
// probes (`thread_count_is_readable`) in the tier that runs that OS.
#[cfg(target_os = "macos")]
#[cfg_attr(test, mutants::skip)] // not compiled on the Linux mutation run
pub(crate) fn count() -> std::io::Result<usize> {
    // SAFETY: `proc_taskinfo` is a C struct of integers and arrays, for which
    // all-zero bytes are valid.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: writes up to `size` bytes into the zeroed, correctly sized
    // proc_taskinfo; the return value is the byte count actually written, or
    // `<= 0` on error. The `written < size` check below rejects both a short
    // write and the negative error sentinel in one comparison.
    let written = unsafe {
        libc::proc_pidinfo(
            nix::unistd::getpid().as_raw(),
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        )
    };
    if written < size {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info.pti_threadnum.max(0) as usize)
}

#[cfg(target_os = "freebsd")]
#[cfg_attr(test, mutants::skip)] // see the note above the macOS query
pub(crate) fn count() -> std::io::Result<usize> {
    // SAFETY: `kinfo_proc` is a C struct of integers and arrays, for which
    // all-zero bytes are valid.
    let mut kp: libc::kinfo_proc = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::kinfo_proc>();
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        nix::unistd::getpid().as_raw(),
    ];
    // SAFETY: `mib` holds `mib.len()` names, and `kp` is writable for `size`
    // bytes, so the kernel writes only inside it.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            (&mut kp as *mut libc::kinfo_proc).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(kp.ki_numthreads.max(0) as usize)
}

#[cfg(target_os = "netbsd")]
#[cfg_attr(test, mutants::skip)] // see the note above the macOS query
pub(crate) fn count() -> std::io::Result<usize> {
    // SAFETY: `kinfo_proc2` is a C struct of integers and arrays, for which
    // all-zero bytes are valid.
    let mut kp: libc::kinfo_proc2 = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::kinfo_proc2>();
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC2,
        libc::KERN_PROC_PID,
        nix::unistd::getpid().as_raw(),
        size as libc::c_int,
        1,
    ];
    // SAFETY: `mib` holds `mib.len()` names, and `kp` is writable for `size`
    // bytes, one record, so the kernel writes only inside it.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            (&mut kp as *mut libc::kinfo_proc2).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(kp.p_nlwps as usize) // p_nlwps is u64
}

#[cfg(target_os = "openbsd")]
#[cfg_attr(test, mutants::skip)] // see the note above the macOS query
pub(crate) fn count() -> std::io::Result<usize> {
    let elem = std::mem::size_of::<libc::kinfo_proc>();

    // Sizing call (oldp = null). With KERN_PROC_SHOW_THREADS the kernel emits
    // one record per thread, but the sizing call may pad the result, so it is
    // only an upper bound on the buffer to allocate.
    let mut size: libc::size_t = 0;
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID | libc::KERN_PROC_SHOW_THREADS,
        nix::unistd::getpid().as_raw(),
        elem as libc::c_int,
        0,
    ];
    // SAFETY: `mib` holds `mib.len()` names, and a null output buffer makes
    // the kernel only store the needed length in `size`.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // A live process always has at least its own thread, so a zero-size sizing
    // result is an anomaly, not a real "zero threads". Surface it as an error
    // rather than Ok(0): daemonize treats an undeterminable count as a
    // hard failure, which is the safe response to a count it cannot trust.
    if size == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sysctl(KERN_PROC_SHOW_THREADS) returned no thread records",
        ));
    }

    // Fetch call into a real buffer. `mib[5]` is the number of records the
    // buffer can hold; after a successful call `size` is the bytes *actually*
    // written, so `size / elem` is the exact thread count (no padding).
    //
    // Known limitation: if a thread is *created* in the window between the
    // sizing call above and this fetch, the kernel truncates to the buffer
    // capacity and reports the stale (smaller) count, so this can under-count.
    // That is the unsafe direction for the single-thread guard, but it only
    // occurs if the caller races thread creation against `daemonize` or a
    // `daemonize_with` init — which already violates their documented "no threads"
    // contract. Closing it fully would need a bounded sizing/fetch retry loop;
    // OpenBSD is a tier-3 target without CI coverage of that path, so it is
    // documented rather than hardened here.
    let mut buf = vec![0u8; size];
    mib[5] = (size / elem) as libc::c_int;
    // SAFETY: `mib` holds `mib.len()` names, and `buf` is writable for `size`
    // bytes, so the kernel writes only inside it.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(size / elem)
}

/// Returns the panic message if `count` is not exactly one thread, else `None`.
///
/// The checked entry points require *exactly* one thread (R45): forking — or
/// calling `setenv` during the user switch — in a multi-threaded process is
/// unsound. Any count other than 1 is a violation — including an anomalous `0`,
/// which a healthy process can never report and so signals an unreliable
/// thread-count query. Failing closed keeps the safety guard from
/// green-lighting on a count it cannot trust. `caller` names the operation in
/// the panic message.
#[cfg(blivet_thread_count)]
fn single_threaded_violation(caller: &str, count: usize) -> Option<String> {
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
/// The imperative shell around [`single_threaded_violation`], used by the
/// checked entry points, [`daemonize`] and [`daemonize_with`], before the fork.
#[cfg(blivet_thread_count)]
pub(crate) fn assert_single_threaded(caller: &str) {
    let count = ThreadCount::open()
        .and_then(|source| settled_count(&source, 1))
        .unwrap_or_else(|_| {
            panic!("{caller}: cannot determine thread count to verify single-threadedness")
        });
    if let Some(msg) = single_threaded_violation(caller, count) {
        panic!("{msg}");
    }
}

/// How long a count above the expected one is re-read before it is believed.
///
/// The bound is a worst case: the read loop ends the moment the count settles,
/// so only a thread that is really still running pays it.
#[cfg(blivet_thread_count)]
const SETTLE_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

/// The count from `source`, once threads already joined have left it.
///
/// The kernel drops an exiting thread from its count some time after
/// `pthread_join` returns: about 30µs on an idle macOS host, but over 10ms on
/// a loaded Android emulator. A count above `expected` is read again until it
/// falls to `expected` or [`SETTLE_WINDOW`] passes, so a thread still running
/// is reported that much later, and a joined one is not reported at all. The
/// checks expect 1.
#[cfg(blivet_thread_count)]
fn settled_count(source: &ThreadCount, expected: usize) -> std::io::Result<usize> {
    settle(|| source.read(), expected, SETTLE_WINDOW)
}

/// [`settled_count`] over any reading, so a test can supply a slow kernel.
#[cfg(blivet_thread_count)]
fn settle(
    mut read: impl FnMut() -> std::io::Result<usize>,
    expected: usize,
    window: std::time::Duration,
) -> std::io::Result<usize> {
    let deadline = std::time::Instant::now() + window;
    loop {
        let n = read()?;
        if n <= expected || std::time::Instant::now() >= deadline {
            return Ok(n);
        }
        // A sleep, not a yield: a thread that is really running holds the
        // loop for the whole window, which a yield would spend spinning.
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// `init`, then the thread check a user switch's `setenv` needs (R126).
///
/// A group-only switch calls no `setenv`, so it is not checked.
#[cfg(blivet_thread_count)]
pub(crate) fn checked_init<T>(
    config: &DaemonConfig,
    init: impl FnOnce() -> Result<T, DaemonizeError>,
) -> impl FnOnce() -> Result<T, DaemonizeError> {
    let switches_user = config.user.is_some();
    move || {
        // Opened before init, which may change root to a jail without /proc.
        // An error, not a panic: this runs in the daemon, where a panic's
        // message reaches only its stderr, and the launcher prints an error.
        let source = if switches_user {
            Some(ThreadCount::open().map_err(|e| {
                DaemonizeError::SystemError(format!("thread count before init: {e}"))
            })?)
        } else {
            None
        };
        let value = init()?;
        if let Some(source) = source {
            let count = settled_count(&source, 1).map_err(|e| {
                DaemonizeError::SystemError(format!("thread count after init: {e}"))
            })?;
            if count != 1 {
                return Err(DaemonizeError::InitLeftThreads(count));
            }
        }
        Ok(value)
    }
}

#[cfg(all(test, blivet_thread_count))]
mod tests {
    use super::*;
    // Covers: R45
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
        let msg = single_threaded_violation("daemonize_with", 2).expect("2 threads is a violation");
        assert!(
            msg.contains("daemonize_with: 2 threads running (expected 1)"),
            "message should name the caller, count, and expectation, got: {msg}"
        );
    }

    // Covers: R126
    //
    // The test harness runs more than one thread, so a check that runs fails.
    #[test]
    fn checked_init_verifies_one_thread_after_init_for_a_user_switch() {
        let mut config = DaemonConfig::new();
        config.user("nobody");
        let init = checked_init(&config, || Ok(()));

        // The test harness runs more than one thread, so the check fails, and
        // as an error the launcher can print, not a panic it never sees.
        match init() {
            Err(DaemonizeError::InitLeftThreads(n)) => assert!(n > 1, "{n}"),
            other => panic!("expected InitLeftThreads, got {other:?}"),
        }
    }

    // Covers: R126
    #[test]
    fn checked_init_skips_the_check_without_a_user_switch() {
        for group in [None, Some("99999")] {
            let mut config = DaemonConfig::new();
            if let Some(group) = group {
                config.group(group);
            }
            let init = checked_init(&config, || Ok(7));
            assert_eq!(
                init().unwrap(),
                7,
                "group {group:?}: no setenv, so no check"
            );
        }
    }

    use crate::test_support::{is_subprocess, run_in_subprocess};

    // Covers: R126
    #[test]
    #[ignore = "needs root to chroot: the privileged tier runs it"]
    fn the_check_after_init_reads_the_count_inside_a_chroot() {
        run_in_subprocess(
            "thread_count::tests::the_check_after_init_reads_the_count_inside_a_chroot_subprocess",
        );
    }

    /// An init that chroots into a jail without `/proc` (ADR 0005) must not
    /// cost the check its count. The harness keeps a second thread, so a
    /// count that was read reports `InitLeftThreads`; one that could not be
    /// read reports `SystemError`.
    #[test]
    #[ignore]
    fn the_check_after_init_reads_the_count_inside_a_chroot_subprocess() {
        if !is_subprocess() {
            return;
        }
        let jail = tempfile::tempdir().unwrap();
        let root = jail.path().to_path_buf();
        let mut config = DaemonConfig::new();
        config.user("nobody");
        let init = checked_init(&config, move || {
            nix::unistd::chroot(&root).unwrap();
            nix::unistd::chdir("/").unwrap();
            Ok(())
        });

        match init() {
            Err(DaemonizeError::InitLeftThreads(n)) => assert!(n > 1, "{n}"),
            other => panic!("expected the count to be read, got {other:?}"),
        }
    }

    // Covers: R45, R126
    #[test]
    fn a_count_that_drops_late_still_settles() {
        let start = std::time::Instant::now();
        let slow_kernel = || {
            let late = start.elapsed() < std::time::Duration::from_millis(50);
            Ok(if late { 3 } else { 1 })
        };
        assert_eq!(settle(slow_kernel, 1, SETTLE_WINDOW).unwrap(), 1);
    }

    // Covers: R45
    #[test]
    fn a_count_that_never_drops_is_reported_after_the_window() {
        let window = std::time::Duration::from_millis(20);
        let start = std::time::Instant::now();
        assert_eq!(settle(|| Ok(3), 1, window).unwrap(), 3);
        assert!(start.elapsed() >= window);
    }

    // Covers: R45, R126
    #[test]
    fn a_joined_thread_has_left_the_settled_count() {
        run_in_subprocess(
            "thread_count::tests::a_joined_thread_has_left_the_settled_count_subprocess",
        );
    }

    /// The kernel drops an exited thread from its count some microseconds
    /// after `pthread_join` returns, so one reading right after the join
    /// often still counts it. The harness keeps a thread of its own, so the
    /// count settles to this process's baseline rather than to 1.
    #[test]
    #[ignore]
    fn a_joined_thread_has_left_the_settled_count_subprocess() {
        if !is_subprocess() {
            return;
        }
        let source = ThreadCount::open().unwrap();
        let base = source.read().unwrap();
        for round in 0..1000 {
            std::thread::spawn(|| {}).join().unwrap();
            assert_eq!(settled_count(&source, base).unwrap(), base, "round {round}");
        }
    }

    /// The thread count backing `daemonize` must reflect the kernel's
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

        let source = ThreadCount::open().expect("thread count should be readable");
        let base = source.read().expect("thread count should be readable");
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

        let with_threads = source.read().expect("thread count should be readable");
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
