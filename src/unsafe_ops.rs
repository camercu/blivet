//! Unsafe code containment zone.
//!
//! Contains safe wrappers around libc/std functions whose invariants this
//! module upholds internally, so other modules call them without
//! `#[allow(unsafe_code)]`.
//!
//! Some `unsafe` operations are *not* encapsulable here because their safety is
//! context-dependent — single-threadedness the caller must establish — so they
//! live at the call site that owns that contract, not behind a fake-safe
//! wrapper:
//! - the `fork` call ([`Forker::fork`](crate::forker::Forker::fork) wrapping
//!   `nix::unistd::fork`) in `forker.rs`, invoked from `daemonize_inner` in
//!   `lib.rs`
//! - `std::env::set_var` (USER/HOME/LOGNAME) in `drop_privileges_unchecked`
//!   (`context.rs`) and the post-fork env step `set_env_vars` (`steps.rs`)
//! - `nix::sys::signal::sigaction` in test code
//!
//! The CLI binary (`main.rs`) is a separate crate root that cannot reach this
//! module, so it carries its own documented `unsafe` blocks: the
//! `daemonize_unchecked` / `drop_privileges_unchecked` calls (single-threaded
//! contract) and the pre-exec `libc::signal(SIGPIPE, SIG_DFL)` reset (R128).

#![allow(unsafe_code)]

/// Reset signal dispositions from 1 through the signal ceiling to SIG_DFL.
///
/// On Linux, iterates standard signals (1..32) then real-time signals
/// (SIGRTMIN..=SIGRTMAX), skipping the NPTL-reserved range (32..SIGRTMIN).
/// On other platforms, iterates 1..=64 and silently skips EINVAL.
///
/// SIGKILL and SIGSTOP are always skipped (cannot be caught/reset).
///
/// SIGPIPE is also skipped, preserving the caller's disposition (R127): the
/// Rust runtime installs SIG_IGN so writes to a closed pipe/socket return
/// `EPIPE` instead of killing the process, and resetting it would silently
/// revoke that for the entire daemon — including the library's own
/// `notify_parent` write, whose documented `NotifyFailed` error could then
/// never be observed. The CLI restores SIG_DFL just before `exec` (R128), so
/// exec'd programs still start with the conventional disposition.
pub(crate) fn reset_signal_dispositions() {
    let signals = signal_range();
    for sig in signals {
        if sig == libc::SIGKILL || sig == libc::SIGSTOP || sig == libc::SIGPIPE {
            continue;
        }

        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = libc::SIG_DFL;
        unsafe { libc::sigemptyset(&mut sa.sa_mask) };
        sa.sa_flags = 0;

        let ret = unsafe { libc::sigaction(sig, &sa, std::ptr::null_mut()) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EINVAL) {
                panic!("sigaction({sig}) failed: {err}");
            }
            // EINVAL: signal cannot be caught, skip
        }
    }
}

/// Returns the range of signal numbers to reset.
///
/// On Linux, returns standard signals (1..32) chained with real-time signals
/// (SIGRTMIN..=SIGRTMAX), deliberately skipping the NPTL-reserved range
/// (typically 32-33) that sits between standard and real-time signals.
///
/// On other platforms, returns 1..=64 (EINVAL skips invalid ones).
fn signal_range() -> Vec<i32> {
    #[cfg(target_os = "linux")]
    {
        let rtmin = libc::SIGRTMIN();
        let rtmax = libc::SIGRTMAX();
        (1..32).chain(rtmin..=rtmax).collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS and other BSDs don't have real-time signals; 31 standard signals
        // but we iterate up to 64 to be safe (EINVAL will skip invalid ones)
        (1..=64).collect()
    }
}

/// Close a file descriptor, ignoring errors.
///
/// Used by `close_inherited_fds` which iterates 3..max_fd and closes
/// speculatively — most fds aren't open, so EBADF is the common case.
/// `nix::unistd::close` can't be used here because it requires `IntoRawFd`
/// (no safe conversion from a bare `i32`), and it treats EBADF as an error.
pub(crate) fn raw_close(fd: i32) {
    unsafe { libc::close(fd) };
}

/// Safe wrapper around `libc::initgroups`.
///
/// `nix::unistd::initgroups` is not available on macOS, so we call libc directly.
pub(crate) fn raw_initgroups(
    user: &std::ffi::CStr,
    group: libc::gid_t,
) -> Result<(), nix::errno::Errno> {
    let ret = unsafe { libc::initgroups(user.as_ptr(), group as _) };
    if ret < 0 {
        Err(nix::errno::Errno::last())
    } else {
        Ok(())
    }
}

/// Terminate the process immediately via `_exit(2)`.
///
/// Unlike `std::process::exit`, this does not run atexit handlers or flush
/// stdio buffers — necessary post-fork to avoid double-flush corruption.
pub(crate) fn raw_exit(code: i32) -> ! {
    unsafe { libc::_exit(code) }
}

/// Path to unlink from inside the async-signal-safe cleanup handler.
///
/// Set by [`install_pidfile_cleanup_signals`] to a leaked, NUL-terminated C
/// string that lives for the rest of the process, so the handler can read it
/// without touching freed memory. The handler only loads this pointer.
static CLEANUP_PIDFILE: std::sync::atomic::AtomicPtr<libc::c_char> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Async-signal-safe handler that removes the pidfile, then re-raises the
/// signal so the default action (process termination) still runs.
extern "C" fn pidfile_cleanup_handler(signum: i32) {
    use std::sync::atomic::Ordering;

    let ptr = CLEANUP_PIDFILE.load(Ordering::Acquire);
    if !ptr.is_null() {
        // SAFETY: `ptr` is null or a valid NUL-terminated C string leaked in
        // `install_pidfile_cleanup_signals` (never freed), so it stays valid
        // for the life of the process. `unlink` is async-signal-safe.
        unsafe { libc::unlink(ptr) };
    }
    // The disposition was reset to default by SA_RESETHAND before this handler
    // ran; re-raise so the process terminates with a status reflecting the
    // signal. `raise` is async-signal-safe.
    unsafe { libc::raise(signum) };
}

/// Install [`pidfile_cleanup_handler`] for each signal in `signals`.
///
/// Stores `pidfile` (leaked, so it outlives any later free) for the handler to
/// unlink. Uses `SA_RESETHAND` so the handler runs once and the re-raise hits
/// the default action.
///
/// All-or-nothing (R129): if `sigaction` fails for any signal (e.g. EINVAL
/// for one that cannot be caught, like SIGKILL/SIGSTOP), the dispositions
/// already replaced for earlier signals in the slice and the prior pidfile
/// pointer are restored, and the returned error names the failing signal.
pub(crate) fn install_pidfile_cleanup_signals(
    pidfile: &std::ffi::CStr,
    signals: &[i32],
) -> std::io::Result<()> {
    use std::sync::atomic::Ordering;

    // Leak a stable copy of the path. Repeated installs leak the prior copy,
    // a small bounded cost for a rarely-repeated setup call. The prior pointer
    // (null or an earlier leaked path, both valid forever) is kept for rollback.
    let leaked: *mut libc::c_char = pidfile.to_owned().into_raw();
    let prior_path = CLEANUP_PIDFILE.swap(leaked, Ordering::AcqRel);

    // Dispositions replaced so far, so a failure can restore them.
    let mut replaced: Vec<(i32, libc::sigaction)> = Vec::with_capacity(signals.len());

    for &sig in signals {
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        // Coerce to a fn pointer before the usize cast (a bare fn-item cast
        // trips clippy::fn_to_numeric_cast); sa_sigaction is pointer-sized.
        let handler = pidfile_cleanup_handler as extern "C" fn(i32);
        sa.sa_sigaction = handler as usize;
        unsafe { libc::sigemptyset(&mut sa.sa_mask) };
        sa.sa_flags = libc::SA_RESETHAND;

        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::sigaction(sig, &sa, &mut old) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            for (s, prev) in replaced.iter().rev() {
                // SAFETY: `prev` is the exact disposition sigaction reported
                // for `s` moments ago; re-installing it is always valid.
                unsafe { libc::sigaction(*s, prev, std::ptr::null_mut()) };
            }
            CLEANUP_PIDFILE.store(prior_path, Ordering::Release);
            return Err(std::io::Error::new(
                err.kind(),
                format!("signal {sig}: {err}"),
            ));
        }
        replaced.push((sig, old));
    }
    Ok(())
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
#[cfg(target_os = "macos")]
pub(crate) fn thread_count() -> std::io::Result<usize> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: writes up to `size` bytes into the zeroed, correctly sized
    // proc_taskinfo; the return value is the byte count actually written.
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
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
pub(crate) fn thread_count() -> std::io::Result<usize> {
    let mut kp: libc::kinfo_proc = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::kinfo_proc>();
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        unsafe { libc::getpid() },
    ];
    // SAFETY: standard sysctl call; `kp`/`size` are a correctly sized output
    // buffer and its length.
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
pub(crate) fn thread_count() -> std::io::Result<usize> {
    let mut kp: libc::kinfo_proc2 = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::kinfo_proc2>();
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC2,
        libc::KERN_PROC_PID,
        unsafe { libc::getpid() },
        size as libc::c_int,
        1,
    ];
    // SAFETY: standard sysctl call with a correctly sized single-record buffer.
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
pub(crate) fn thread_count() -> std::io::Result<usize> {
    let elem = std::mem::size_of::<libc::kinfo_proc>();

    // Sizing call (oldp = null). With KERN_PROC_SHOW_THREADS the kernel emits
    // one record per thread, but the sizing call may pad the result, so it is
    // only an upper bound on the buffer to allocate.
    let mut size: libc::size_t = 0;
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID | libc::KERN_PROC_SHOW_THREADS,
        unsafe { libc::getpid() },
        elem as libc::c_int,
        0,
    ];
    // SAFETY: standard sizing sysctl call; output pointer is null.
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
    let mut buf = vec![0u8; size];
    mib[5] = (size / elem) as libc::c_int;
    // SAFETY: `buf`/`size` are a matching pointer and length.
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

/// Returns a `BorrowedFd` for `AT_FDCWD`, the sentinel that means
/// "resolve relative paths against the current working directory."
pub(crate) fn at_fdcwd() -> std::os::fd::BorrowedFd<'static> {
    // SAFETY: AT_FDCWD is a well-known sentinel value (-100 on Linux, -2 on
    // macOS) that the kernel recognises; it does not alias any real fd.
    unsafe { std::os::fd::BorrowedFd::borrow_raw(libc::AT_FDCWD) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{is_subprocess, run_in_subprocess};
    use std::sync::atomic::Ordering;

    /// Current disposition of `sig` (the `sa_sigaction` word), read without
    /// changing it.
    fn current_disposition(sig: i32) -> usize {
        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::sigaction(sig, std::ptr::null(), &mut old) };
        assert_eq!(ret, 0, "sigaction query for signal {sig} failed");
        old.sa_sigaction
    }

    // Covers: R129
    #[test]
    fn failed_install_rolls_back() {
        run_in_subprocess("unsafe_ops::tests::failed_install_rolls_back_subprocess");
    }

    /// A failing install must leave no trace: handlers installed for signals
    /// earlier in the slice are restored, the failing signal is named in the
    /// error, and the handler's pidfile pointer reverts to its prior value.
    /// Mutates process-global signal state, hence the subprocess isolation.
    #[test]
    #[ignore]
    fn failed_install_rolls_back_subprocess() {
        if !is_subprocess() {
            return;
        }

        // Establish a prior successful install so pointer rollback is
        // distinguishable from "still null".
        install_pidfile_cleanup_signals(c"/tmp/rollback-prior.pid", &[libc::SIGUSR2])
            .expect("SIGUSR2 install should succeed");
        let prior_ptr = CLEANUP_PIDFILE.load(Ordering::Acquire);
        let prior_usr1 = current_disposition(libc::SIGUSR1);

        // SIGUSR1 installs, then SIGKILL fails with EINVAL.
        let err = install_pidfile_cleanup_signals(
            c"/tmp/rollback-new.pid",
            &[libc::SIGUSR1, libc::SIGKILL],
        )
        .expect_err("SIGKILL cannot be caught");

        assert!(
            err.to_string()
                .contains(&format!("signal {}", libc::SIGKILL)),
            "error should name the failing signal, got: {err}"
        );
        assert_eq!(
            current_disposition(libc::SIGUSR1),
            prior_usr1,
            "SIGUSR1 disposition should be rolled back after the failed install"
        );
        assert_eq!(
            CLEANUP_PIDFILE.load(Ordering::Acquire),
            prior_ptr,
            "pidfile pointer should revert to the prior install's path"
        );
        // The prior install must keep working: SIGUSR2 still has the handler.
        assert_eq!(
            current_disposition(libc::SIGUSR2),
            pidfile_cleanup_handler as extern "C" fn(i32) as usize,
            "earlier successful install should be untouched"
        );
    }
}
