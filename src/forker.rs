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
    /// Makes the pipe the daemon reports its start through: `(read, write)`.
    /// Runs in the caller's process, so a failure is an error for the caller.
    fn create_notification_pipe(&mut self) -> Result<(OwnedFd, OwnedFd), DaemonizeError>;
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
    fn create_notification_pipe(&mut self) -> Result<(OwnedFd, OwnedFd), DaemonizeError> {
        use nix::fcntl::{fcntl, FcntlArg, FdFlag};

        let failed = |e| DaemonizeError::SystemError(format!("notification pipe: {e}"));
        let (rd, wr) = nix::unistd::pipe().map_err(failed)?;
        // Set O_CLOEXEC on both ends. pipe2(O_CLOEXEC) would be atomic, but
        // macOS lacks pipe2. The two-step approach is safe here because
        // daemonize() requires single-threaded execution.
        for end in [&rd, &wr] {
            fcntl(end.as_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(failed)?;
        }
        Ok((rd, wr))
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
        pipe: Pipe,
        pipe_reader: Option<OwnedFd>,
    }

    /// What [`NullForker`]'s
    /// [`create_notification_pipe`](Forker::create_notification_pipe) does.
    enum Pipe {
        /// Returns a real pipe, and keeps a copy of its read end.
        Real,
        /// Fails, as a full descriptor table does.
        Fails,
    }

    impl NullForker {
        pub(crate) fn new(
            fork_results: Vec<Result<ForkResult, DaemonizeError>>,
            setsid_result: Result<(), DaemonizeError>,
        ) -> Self {
            Self {
                fork_results: fork_results.into(),
                setsid_result: Some(setsid_result),
                pipe: Pipe::Real,
                pipe_reader: None,
            }
        }

        /// Make [`create_notification_pipe`](Forker::create_notification_pipe)
        /// fail.
        pub(crate) fn with_failing_pipe(mut self) -> Self {
            self.pipe = Pipe::Fails;
            self
        }

        /// Take the test-side duplicate of the pipe's read end, so a test can
        /// observe what the fork sequence writes — or must not write — on the
        /// wire. `daemonize_inner` drops its own
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
        fn create_notification_pipe(&mut self) -> Result<(OwnedFd, OwnedFd), DaemonizeError> {
            match self.pipe {
                Pipe::Fails => Err(DaemonizeError::SystemError(
                    "notification pipe: injected failure".into(),
                )),
                Pipe::Real => {
                    let (rd, wr) = nix::unistd::pipe().expect("failed to create test pipe");
                    self.pipe_reader = Some(rd.try_clone().expect("failed to dup test read end"));
                    Ok((rd, wr))
                }
            }
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

        const NAME: &str = "forker::tests::real_setsid_makes_the_caller_session_leader";
        let output = crate::test_support::rerun_in_subprocess(NAME, MARKER, "1");
        assert!(
            output.status.success(),
            "{}",
            crate::test_support::subprocess_report(NAME, &output)
        );
    }

    // Covers: R148
    #[test]
    fn notification_pipe_with_no_descriptor_free_is_an_error() {
        crate::test_support::run_in_subprocess(
            "forker::tests::notification_pipe_with_no_descriptor_free_is_an_error_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn notification_pipe_with_no_descriptor_free_is_an_error_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let held = crate::test_support::fill_fd_table(0);

        let result = RealForker.create_notification_pipe();
        drop(held);

        assert!(
            matches!(result, Err(DaemonizeError::SystemError(_))),
            "{result:?}"
        );
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
