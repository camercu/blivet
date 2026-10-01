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
//! - `std::env::set_var` (USER/HOME/LOGNAME) in `ResolvedIdentity::switch_to`
//!   (`identity.rs`) and the post-fork env step `set_env_vars` (`steps.rs`)
//! - `nix::sys::signal::sigaction` in test code
//!
//! The CLI binary (`main.rs`) is a separate crate root that cannot reach this
//! module, so it carries its own documented `unsafe` blocks: the
//! `daemonize_unchecked` call (single-threaded contract) and the pre-exec `libc::signal(SIGPIPE, SIG_DFL)` reset (R128).

#![allow(unsafe_code)]
/// Close a file descriptor by number, ignoring errors.
///
/// For a descriptor nothing owns: `close_inherited_fds` closes `3..max_fd`
/// speculatively, where most fds aren't open and EBADF is the common case, and
/// step 12 recloses a stdio slot that was closed before it moved.
/// `nix::unistd::close` can't be used because it requires `IntoRawFd` (no safe
/// conversion from a bare `i32`), and it treats EBADF as an error.
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

/// Returns a `BorrowedFd` for `AT_FDCWD`, the sentinel that means
/// "resolve relative paths against the current working directory."
pub(crate) fn at_fdcwd() -> std::os::fd::BorrowedFd<'static> {
    // SAFETY: AT_FDCWD is a well-known sentinel value (-100 on Linux, -2 on
    // macOS) that the kernel recognises; it does not alias any real fd.
    unsafe { std::os::fd::BorrowedFd::borrow_raw(libc::AT_FDCWD) }
}
