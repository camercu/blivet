//! Forker trait abstracting fork/setsid/pipe syscalls for testability.
//!
//! [`RealForker`](crate::unsafe_ops::RealForker) wraps real syscalls;
//! [`NullForker`](null_forker::NullForker) (test-only) provides
//! configurable results so `daemonize_inner` can be exercised without forking.

use std::os::fd::OwnedFd;

use nix::unistd::ForkResult;

use crate::error::DaemonizeError;

/// Abstraction over fork/setsid/pipe for testability.
///
/// `daemonize_inner` is generic over this trait. `RealForker` wraps real
/// syscalls; `NullForker` (test-only) provides configurable results.
pub(crate) trait Forker {
    fn create_notification_pipe(&mut self) -> Option<(OwnedFd, OwnedFd)>;
    fn fork(&mut self) -> Result<ForkResult, DaemonizeError>;
    fn setsid(&mut self) -> Result<(), DaemonizeError>;
    fn exit(&self, code: i32) -> !;
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
            Self::new(
                vec![
                    Ok(ForkResult::Child),
                    Ok(ForkResult::Child),
                ],
                Ok(()),
            )
        }

        /// First fork returns Parent.
        pub(crate) fn first_parent() -> Self {
            Self::new(
                vec![Ok(ForkResult::Parent { child: Pid::from_raw(42) })],
                Ok(()),
            )
        }

        /// First fork Child, second fork Parent.
        pub(crate) fn second_parent() -> Self {
            Self::new(
                vec![
                    Ok(ForkResult::Child),
                    Ok(ForkResult::Parent { child: Pid::from_raw(43) }),
                ],
                Ok(()),
            )
        }

        /// First fork fails.
        pub(crate) fn first_fork_fails() -> Self {
            Self::new(
                vec![Err(DaemonizeError::ForkFailed("first fork failed".into()))],
                Ok(()),
            )
        }

        /// Setsid fails.
        pub(crate) fn setsid_fails() -> Self {
            Self::new(
                vec![Ok(ForkResult::Child)],
                Err(DaemonizeError::SetsidFailed("setsid failed".into())),
            )
        }

        /// Second fork fails.
        pub(crate) fn second_fork_fails() -> Self {
            Self::new(
                vec![
                    Ok(ForkResult::Child),
                    Err(DaemonizeError::ForkFailed("second fork failed".into())),
                ],
                Ok(()),
            )
        }
    }

    impl Forker for NullForker {
        fn create_notification_pipe(&mut self) -> Option<(OwnedFd, OwnedFd)> {
            // NullForker skips the pipe; parent exits immediately
            None
        }

        fn fork(&mut self) -> Result<ForkResult, DaemonizeError> {
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
