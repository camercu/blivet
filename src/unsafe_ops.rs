#![allow(unsafe_code)]

use std::os::fd::OwnedFd;

use nix::unistd::ForkResult;

use crate::error::DaemonizeError;
use crate::forker::Forker;

/// Production forker that wraps real syscalls.
pub(crate) struct RealForker;

impl Forker for RealForker {
    fn create_notification_pipe(&mut self) -> Option<(OwnedFd, OwnedFd)> {
        use nix::fcntl::{fcntl, FcntlArg, FdFlag};

        let (rd, wr) = nix::unistd::pipe()
            .expect("failed to create notification pipe");
        // Set O_CLOEXEC on both ends
        fcntl(rd.as_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))
            .expect("failed to set CLOEXEC on pipe read end");
        fcntl(wr.as_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))
            .expect("failed to set CLOEXEC on pipe write end");
        Some((rd, wr))
    }

    fn fork(&mut self) -> Result<ForkResult, DaemonizeError> {
        match unsafe { nix::unistd::fork() } {
            Ok(result) => Ok(result),
            Err(e) => Err(DaemonizeError::ForkFailed(format!("fork failed: {e}"))),
        }
    }

    fn setsid(&mut self) -> Result<(), DaemonizeError> {
        nix::unistd::setsid()
            .map(|_| ())
            .map_err(|e| DaemonizeError::SetsidFailed(format!("setsid failed: {e}")))
    }

    fn exit(&self, code: i32) -> ! {
        unsafe { libc::_exit(code) }
    }
}

use std::os::fd::AsFd;

/// Reset signal dispositions from 1 through the signal ceiling to SIG_DFL.
///
/// Skips SIGKILL and SIGSTOP (cannot be caught/reset). If `sigaction` returns
/// EINVAL (e.g., NPTL-reserved signals 32-33), the signal is silently skipped.
pub(crate) fn reset_signal_dispositions() {
    let max = signal_ceiling();
    for sig in 1..=max {
        // Skip SIGKILL (9) and SIGSTOP (19)
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
            // EINVAL: signal cannot be caught (e.g., NPTL-reserved), skip
        }
    }
}

/// Returns the signal ceiling for signal reset iteration.
///
/// On Linux, uses `libc::SIGRTMAX()`. On other platforms, falls back to 64.
pub(crate) fn signal_ceiling() -> i32 {
    #[cfg(target_os = "linux")]
    {
        unsafe { libc::SIGRTMAX() }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS and other BSDs don't have real-time signals; 31 standard signals
        // but we iterate up to 64 to be safe (EINVAL will skip invalid ones)
        64
    }
}

/// Returns the minimum real-time signal number.
///
/// On Linux, uses `libc::SIGRTMIN()`. On other platforms, returns 0 (no RT signals).
#[cfg(target_os = "linux")]
pub(crate) fn sigrtmin() -> i32 {
    unsafe { libc::SIGRTMIN() }
}

/// Safe wrapper around `libc::dup2`. Duplicates `oldfd` onto `newfd`.
///
/// Returns `Ok(newfd)` on success, `Err(errno)` on failure.
pub(crate) fn raw_dup2(oldfd: i32, newfd: i32) -> Result<i32, nix::errno::Errno> {
    let ret = unsafe { libc::dup2(oldfd, newfd) };
    if ret < 0 {
        Err(nix::errno::Errno::last())
    } else {
        Ok(ret)
    }
}

/// Safe wrapper around `libc::close`.
pub(crate) fn raw_close(fd: i32) {
    unsafe { libc::close(fd) };
}

/// Safe wrapper around `libc::open`.
pub(crate) fn raw_open(path: &std::ffi::CStr, flags: i32, mode: libc::mode_t) -> Result<i32, nix::errno::Errno> {
    let ret = unsafe { libc::open(path.as_ptr(), flags, mode as libc::c_uint) };
    if ret < 0 {
        Err(nix::errno::Errno::last())
    } else {
        Ok(ret)
    }
}

/// Safe wrapper around `libc::lseek` to seek to a given offset from start.
pub(crate) fn raw_lseek(fd: i32, offset: i64) -> Result<i64, nix::errno::Errno> {
    let ret = unsafe { libc::lseek(fd, offset as libc::off_t, libc::SEEK_SET) };
    if ret < 0 {
        Err(nix::errno::Errno::last())
    } else {
        Ok(ret as i64)
    }
}

/// Safe wrapper around `libc::ftruncate`.
pub(crate) fn raw_ftruncate(fd: i32, length: i64) -> Result<(), nix::errno::Errno> {
    let ret = unsafe { libc::ftruncate(fd, length as libc::off_t) };
    if ret < 0 {
        Err(nix::errno::Errno::last())
    } else {
        Ok(())
    }
}

/// Safe wrapper around `libc::write`.
pub(crate) fn raw_write(fd: i32, buf: &[u8]) -> Result<usize, nix::errno::Errno> {
    let ret = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
    if ret < 0 {
        Err(nix::errno::Errno::last())
    } else {
        Ok(ret as usize)
    }
}

/// Safe wrapper around `libc::initgroups`.
pub(crate) fn raw_initgroups(user: &std::ffi::CStr, group: libc::gid_t) -> Result<(), nix::errno::Errno> {
    let ret = unsafe { libc::initgroups(user.as_ptr(), group as _) };
    if ret < 0 {
        Err(nix::errno::Errno::last())
    } else {
        Ok(())
    }
}
