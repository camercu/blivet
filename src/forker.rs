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
        use_pipe: bool,
        pipe_reader: Option<OwnedFd>,
    }

    impl NullForker {
        pub(crate) fn new(
            fork_results: Vec<Result<ForkResult, DaemonizeError>>,
            setsid_result: Result<(), DaemonizeError>,
        ) -> Self {
            Self {
                fork_results: fork_results.into(),
                setsid_result: Some(setsid_result),
                use_pipe: false,
                pipe_reader: None,
            }
        }

        /// Make [`create_notification_pipe`](Forker::create_notification_pipe)
        /// return a real pipe instead of `None`, so a test can observe what
        /// the fork sequence writes — or must not write — on the wire.
        pub(crate) fn with_pipe(mut self) -> Self {
            self.use_pipe = true;
            self
        }

        /// Take the test-side duplicate of the pipe's read end (created by
        /// [`with_pipe`](Self::with_pipe)). `daemonize_inner` drops its own
        /// read-end copy in the child branch; this duplicate lets the test
        /// read what reached the pipe afterwards.
        pub(crate) fn take_pipe_reader(&mut self) -> Option<OwnedFd> {
            self.pipe_reader.take()
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
            if !self.use_pipe {
                // Default: skip the pipe; the parent branch exits immediately.
                return None;
            }
            let (rd, wr) = nix::unistd::pipe().expect("failed to create test pipe");
            self.pipe_reader = Some(rd.try_clone().expect("failed to dup test read end"));
            Some((rd, wr))
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

#[cfg(test)]
mod tests {
    use super::*;
    use nix::fcntl::{fcntl, FcntlArg, FdFlag};
    use std::os::fd::AsFd;

    // setsid fails with EPERM when the caller already leads a process group,
    // so run it in a self-spawned child (env marker), which inherits the
    // parent's process group and is never a leader.
    #[test]
    fn real_setsid_makes_the_caller_session_leader() {
        const MARKER: &str = "__BLIVET_REAL_SETSID";

        if std::env::var(MARKER).is_ok() {
            RealForker.setsid().expect("setsid should succeed");
            let pid = nix::unistd::getpid();
            let sid = nix::unistd::getsid(None).unwrap();
            assert_eq!(sid, pid, "caller should lead the new session");
            return;
        }

        let exe = std::env::current_exe().unwrap();
        let status = std::process::Command::new(exe)
            .arg("--exact")
            .arg("forker::tests::real_setsid_makes_the_caller_session_leader")
            .arg("--nocapture")
            .env(MARKER, "1")
            .status()
            .unwrap();
        assert!(status.success(), "subprocess assertions failed");
    }

    // Covers: R107 — both notification pipe ends are created with O_CLOEXEC, so
    // the daemon's exec does not leak them to the target program.
    #[test]
    fn notification_pipe_ends_have_cloexec() {
        let (rd, wr) = RealForker
            .create_notification_pipe()
            .expect("RealForker creates a pipe");
        for fd in [rd.as_fd(), wr.as_fd()] {
            let flags = fcntl(fd, FcntlArg::F_GETFD).expect("F_GETFD");
            assert!(
                FdFlag::from_bits_truncate(flags).contains(FdFlag::FD_CLOEXEC),
                "notification pipe end must have FD_CLOEXEC set"
            );
        }
    }
}
