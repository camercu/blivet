//! Thin FFI wrappers with no concept of their own to live in: closing a
//! descriptor by number, `initgroups`, `_exit`, and `AT_FDCWD`.
//!
//! Each upholds its own invariants, so callers use it without `unsafe`. Raw
//! FFI that belongs to a concept lives with it instead: signal handling in
//! `signals.rs`, the per-OS thread counts in `thread_count.rs`. `unsafe` whose
//! soundness depends on a single-threaded caller stays at the call site that
//! owns that contract: `fork` in `forker.rs`, `setenv` in `identity.rs` and
//! `steps/process.rs`, the sequence in `sequence.rs`, and the entry points in
//! `lib.rs`.

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
    nix::errno::Errno::result(ret).map(drop)
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
