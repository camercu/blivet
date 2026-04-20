//! Forker trait abstracting fork/setsid/pipe syscalls for testability.
//!
//! [`RealForker`] wraps real syscalls;
//! [`NullForker`](null_forker::NullForker) (test-only) provides
//! configurable results so `daemonize_inner` can be exercised without forking.

use std::os::fd::{AsFd, OwnedFd};

use nix::unistd::ForkResult;

use crate::error::DaemonizeError;
use crate::unsafe_ops;

/// Abstraction over fork/setsid/pipe for testability.
///
/// `daemonize_inner` is generic over this trait. `RealForker` wraps real
/// syscalls; `NullForker` (test-only) provides configurable results.
#[allow(unsafe_code)]
pub(crate) trait Forker {
    fn create_notification_pipe(&mut self) -> Option<(OwnedFd, OwnedFd)>;
    /// # Safety
    ///
    /// Calling `fork()` in a multithreaded process is undefined behavior.
    /// The caller must ensure no other threads exist.
    unsafe fn fork(&mut self) -> Result<ForkResult, DaemonizeError>;
    fn setsid(&mut self) -> Result<(), DaemonizeError>;
    fn exit(&self, code: i32) -> !;
}

/// Production forker that wraps real syscalls.
pub(crate) struct RealForker;

#[allow(unsafe_code)]
impl Forker for RealForker {
    fn create_notification_pipe(&mut self) -> Option<(OwnedFd, OwnedFd)> {
        use nix::fcntl::{fcntl, FcntlArg, FdFlag};

        let (rd, wr) = nix::unistd::pipe().expect("failed to create notification pipe");
        // Set O_CLOEXEC on both ends. pipe2(O_CLOEXEC) would be atomic, but
        // macOS lacks pipe2. The two-step approach is safe here because
        // daemonize() requires single-threaded execution.
        fcntl(rd.as_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))
            .expect("failed to set CLOEXEC on pipe read end");
        fcntl(wr.as_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))
            .expect("failed to set CLOEXEC on pipe write end");
        Some((rd, wr))
    }

    unsafe fn fork(&mut self) -> Result<ForkResult, DaemonizeError> {
        match nix::unistd::fork() {
            Ok(result) => Ok(result),
            Err(e) => Err(DaemonizeError::ForkFailed(e.to_string())),
        }
    }

    fn setsid(&mut self) -> Result<(), DaemonizeError> {
        nix::unistd::setsid()
            .map(|_| ())
            .map_err(|e| DaemonizeError::SetsidFailed(e.to_string()))
    }

    fn exit(&self, code: i32) -> ! {
        unsafe_ops::raw_exit(code)
    }
}

#[cfg(test)]
pub(crate) mod null_forker {
    use super::*;
    use nix::unistd::{ForkResult, Pid};
    use std::collections::VecDeque;

    /// Test double for `Forker`. `exit()` panics so tests can use `catch_unwind`.
    pub(crate) struct NullForker {
        fork_results: VecDeque<Result<ForkResult, DaemonizeError>>,
        setsid_result: Option<Result<(), DaemonizeError>>,
    }

    impl NullForker {
        pub(crate) fn new(
            fork_results: Vec<Result<ForkResult, DaemonizeError>>,
            setsid_result: Result<(), DaemonizeError>,
        ) -> Self {
            Self {
                fork_results: fork_results.into(),
                setsid_result: Some(setsid_result),
            }
        }

        /// Both forks return Child.
        pub(crate) fn both_child() -> Self {
            Self::new(vec![Ok(ForkResult::Child), Ok(ForkResult::Child)], Ok(()))
        }

        /// First fork returns Parent.
        pub(crate) fn first_parent() -> Self {
            Self::new(
                vec![Ok(ForkResult::Parent {
                    child: Pid::from_raw(42),
                })],
                Ok(()),
            )
        }

        /// First fork Child, second fork Parent.
        pub(crate) fn second_parent() -> Self {
            Self::new(
                vec![
                    Ok(ForkResult::Child),
                    Ok(ForkResult::Parent {
                        child: Pid::from_raw(43),
                    }),
                ],
                Ok(()),
            )
        }

        /// First fork fails.
        pub(crate) fn first_fork_fails() -> Self {
            Self::new(
                vec![Err(DaemonizeError::ForkFailed("first fork".into()))],
                Ok(()),
            )
        }

        /// Setsid fails.
        pub(crate) fn setsid_fails() -> Self {
            Self::new(
                vec![Ok(ForkResult::Child)],
                Err(DaemonizeError::SetsidFailed("test".into())),
            )
        }

        /// Second fork fails.
        pub(crate) fn second_fork_fails() -> Self {
            Self::new(
                vec![
                    Ok(ForkResult::Child),
                    Err(DaemonizeError::ForkFailed("second fork".into())),
                ],
                Ok(()),
            )
        }
    }

    #[allow(unsafe_code)]
    impl Forker for NullForker {
        fn create_notification_pipe(&mut self) -> Option<(OwnedFd, OwnedFd)> {
            // NullForker skips the pipe; parent exits immediately
            None
        }

        unsafe fn fork(&mut self) -> Result<ForkResult, DaemonizeError> {
            self.fork_results
                .pop_front()
                .expect("NullForker: no more fork results")
        }

        fn setsid(&mut self) -> Result<(), DaemonizeError> {
            self.setsid_result
                .take()
                .expect("NullForker: setsid already consumed")
        }

        fn exit(&self, code: i32) -> ! {
            panic!("NullForker::exit({})", code);
        }
    }
}
