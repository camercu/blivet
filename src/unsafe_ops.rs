//! Unsafe code containment zone.
//!
//! Contains safe wrappers around libc/std functions that require `unsafe`.
//! Other modules call these wrappers without needing `#[allow(unsafe_code)]`.
//!
//! The few `unsafe` blocks outside this module are:
//! - `unsafe fn` calls to [`Forker::fork`](crate::forker::Forker::fork) and
//!   `nix::unistd::fork` in `forker.rs` / `lib.rs`
//! - `nix::sys::signal::sigaction` in test code

#![allow(unsafe_code)]

/// Reset signal dispositions from 1 through the signal ceiling to SIG_DFL.
///
/// On Linux, iterates standard signals (1..32) then real-time signals
/// (SIGRTMIN..=SIGRTMAX), skipping the NPTL-reserved range (32..SIGRTMIN).
/// On other platforms, iterates 1..=64 and silently skips EINVAL.
///
/// SIGKILL and SIGSTOP are always skipped (cannot be caught/reset).
pub(crate) fn reset_signal_dispositions() {
    let signals = signal_range();
    for sig in signals {
        if sig == libc::SIGKILL || sig == libc::SIGSTOP {
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

/// Safe wrapper around `libc::close`.
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

/// Set an environment variable.
///
/// `std::env::set_var` is not thread-safe (unsafe since Rust 1.83).
/// Callers must ensure no other threads exist (e.g. post-fork child).
pub(crate) fn raw_set_env_var(
    key: impl AsRef<std::ffi::OsStr>,
    value: impl AsRef<std::ffi::OsStr>,
) {
    unsafe { std::env::set_var(key, value) };
}

/// Remove an environment variable.
///
/// Same thread-safety constraints as [`raw_set_env_var`].
#[cfg(test)]
pub(crate) fn raw_remove_env_var(key: &str) {
    unsafe { std::env::remove_var(key) };
}

/// Terminate the process immediately via `_exit(2)`.
///
/// Unlike `std::process::exit`, this does not run atexit handlers or flush
/// stdio buffers — necessary post-fork to avoid double-flush corruption.
pub(crate) fn raw_exit(code: i32) -> ! {
    unsafe { libc::_exit(code) }
}

/// Returns a `BorrowedFd` for `AT_FDCWD`, the sentinel that means
/// "resolve relative paths against the current working directory."
pub(crate) fn at_fdcwd() -> std::os::fd::BorrowedFd<'static> {
    // SAFETY: AT_FDCWD is a well-known sentinel value (-100 on Linux, -2 on
    // macOS) that the kernel recognises; it does not alias any real fd.
    unsafe { std::os::fd::BorrowedFd::borrow_raw(libc::AT_FDCWD) }
}
