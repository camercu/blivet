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
/// the default action. Returns the OS error if `sigaction` fails (e.g. EINVAL
/// for a signal that cannot be caught, like SIGKILL/SIGSTOP).
pub(crate) fn install_pidfile_cleanup_signals(
    pidfile: &std::ffi::CStr,
    signals: &[i32],
) -> std::io::Result<()> {
    use std::sync::atomic::Ordering;

    // Leak a stable copy of the path. Repeated installs leak the prior copy,
    // a small bounded cost for a rarely-repeated setup call.
    let leaked: *mut libc::c_char = pidfile.to_owned().into_raw();
    CLEANUP_PIDFILE.store(leaked, Ordering::Release);

    for &sig in signals {
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        // Coerce to a fn pointer before the usize cast (a bare fn-item cast
        // trips clippy::fn_to_numeric_cast); sa_sigaction is pointer-sized.
        let handler = pidfile_cleanup_handler as extern "C" fn(i32);
        sa.sa_sigaction = handler as usize;
        unsafe { libc::sigemptyset(&mut sa.sa_mask) };
        sa.sa_flags = libc::SA_RESETHAND;

        let ret = unsafe { libc::sigaction(sig, &sa, std::ptr::null_mut()) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Number of threads in the current process, for the single-threaded check in
/// [`daemonize_checked`](crate::daemonize_checked) on platforms without
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
    Ok((kp.p_nlwps as i64).max(0) as usize)
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
    if size == 0 {
        return Ok(0);
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
