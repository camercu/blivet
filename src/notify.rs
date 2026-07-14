//! Wire protocol for the parent-readiness notification pipe.
//!
//! After the double fork, the grandchild (the daemon) writes exactly one
//! message to the pipe; the original parent reads it, and both exit. The
//! format is a single leading **exit-code byte**, optionally followed by a
//! UTF-8 message:
//!
//! - `0x00` — success; the daemon is ready. Any trailing bytes are ignored.
//! - non-zero `code` + message — failure; the parent prints `message` to
//!   stderr and exits with `code`.
//! - pipe closed with no bytes (EOF) — the CLI `exec`'d successfully and
//!   `O_CLOEXEC` closed the write end; treated as success.
//!
//! Centralizing the byte layout here keeps the three writers (success, error,
//! drop-without-notify) and the single reader from drifting out of sync.

use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use crate::error::DaemonizeError;

/// The write end of the parent-readiness notification pipe.
///
/// Owns the single act of writing one wire-protocol message (see the module
/// docs) and closing the pipe. The pipe is **one-shot**: each `signal_*` method
/// consumes `self`, after which the write end is closed (the parent reads the
/// message, then EOF).
///
/// Holding this in an `Option` lets a caller treat "still `Some`" as "the daemon
/// has not signalled yet" — which is exactly how [`DaemonContext`] decides, on
/// drop, whether to report an unnotified exit.
///
/// [`DaemonContext`]: crate::DaemonContext
/// The fd is held in an `Option` so the type can distinguish "already spoke on
/// the wire" (`None`) from "never spoke" (`Some`). Every consuming method takes
/// the fd out before writing; [`Drop`] uses the leftover `Some` as the signal
/// to fire the unnotified-failure safety net.
pub(crate) struct NotifyPipe(Option<OwnedFd>);

impl NotifyPipe {
    /// Wrap the write end of a notification pipe.
    pub(crate) fn new(fd: OwnedFd) -> Self {
        NotifyPipe(Some(fd))
    }

    /// Borrow the underlying fd, e.g. to exempt it from
    /// [`close_inherited_fds`](crate::steps::close_inherited_fds).
    pub(crate) fn as_fd(&self) -> BorrowedFd<'_> {
        self.0
            .as_ref()
            .expect("notify pipe fd borrowed after it was consumed")
            .as_fd()
    }

    /// Close the write end without writing any message.
    ///
    /// The intermediate fork processes (the original parent and the first
    /// child) each inherit a copy of the write end but must stay silent — the
    /// grandchild daemon is the sole writer. Closing here takes the fd out so
    /// the [`Drop`] safety net does not fire a spurious failure.
    pub(crate) fn close(mut self) {
        drop(self.0.take());
    }

    /// Signal readiness: write the success byte and close the pipe.
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if writing to the pipe fails.
    #[must_use = "the parent process blocks until notified; ignoring this Result may leave it waiting"]
    pub(crate) fn signal_ready(mut self) -> io::Result<()> {
        self.write_all(&SUCCESS)
    }

    /// Signal a daemonization failure: write the error's exit-code byte and
    /// message, then close the pipe. Best-effort — write errors are ignored
    /// because the process is aborting regardless.
    pub(crate) fn signal_error(mut self, err: &DaemonizeError) {
        let _ = self.write_all(&error_bytes(err));
    }

    /// Signal that the daemon exited without ever calling
    /// [`notify_parent`](crate::DaemonContext::notify_parent). Best-effort.
    pub(crate) fn signal_unnotified(mut self) {
        let _ = self.write_all(&unnotified_bytes());
    }

    /// Take the fd (if still present) and write `bytes` through it, closing it.
    /// A no-op once the fd has been taken, so it is safe to call again from
    /// [`Drop`] after a `signal_*`/`close` has already consumed the pipe.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let Some(fd) = self.0.take() else {
            return Ok(());
        };
        let mut file = io::BufWriter::new(std::fs::File::from(fd));
        file.write_all(bytes)?;
        file.flush()
    }
}

impl Drop for NotifyPipe {
    /// Safety net: a `NotifyPipe` still holding its fd at drop means the daemon
    /// process is going away without having reported an outcome — e.g. a panic
    /// unwound past the signalling seam. Report a failure so the closed pipe is
    /// not decoded as EOF = success, which would tell the parent that a crashed
    /// daemon started cleanly. The intended exits (`signal_*`, `close`) take the
    /// fd first, leaving this a no-op. Never panics, so it is safe during an
    /// unwind.
    fn drop(&mut self) {
        let _ = self.write_all(&unnotified_bytes());
    }
}

/// Exit-code byte written when a [`DaemonContext`](crate::DaemonContext) is
/// dropped before the daemon signalled readiness.
const UNNOTIFIED_CODE: u8 = 1;

/// Message paired with [`UNNOTIFIED_CODE`].
const UNNOTIFIED_MSG: &[u8] = b"daemon exited without signaling readiness";

/// The single success byte (`0x00`).
pub(crate) const SUCCESS: [u8; 1] = [0x00];

/// Encode a failure message: exit-code byte followed by the error's `Display`.
///
/// [`exit_code`](DaemonizeError::exit_code) is always non-zero, so the message
/// can never collide with the [`SUCCESS`] byte.
pub(crate) fn error_bytes(err: &DaemonizeError) -> Vec<u8> {
    encode(err.exit_code(), err.to_string().as_bytes())
}

/// Encode the "dropped without notifying the parent" failure message.
pub(crate) fn unnotified_bytes() -> Vec<u8> {
    encode(UNNOTIFIED_CODE, UNNOTIFIED_MSG)
}

fn encode(code: u8, msg: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + msg.len());
    buf.push(code);
    buf.extend_from_slice(msg);
    buf
}

/// The decoded result of a message read from the notification pipe.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Daemon is ready (success byte) or the pipe hit EOF after a clean exec.
    Success,
    /// Daemon failed; report `message` to stderr and exit with `code`.
    Failure { code: i32, message: String },
}

/// Decode a message read from the notification pipe.
pub(crate) fn decode(buf: &[u8]) -> Outcome {
    match buf.first() {
        // EOF (exec succeeded, CLOEXEC closed the pipe) or explicit success byte.
        None | Some(&0x00) => Outcome::Success,
        Some(&code) => Outcome::Failure {
            code: code as i32,
            message: String::from_utf8_lossy(&buf[1..]).into_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Create a pipe and return its (read, write) ends.
    fn make_pipe() -> (OwnedFd, OwnedFd) {
        nix::unistd::pipe().unwrap()
    }

    /// Drain the read end to EOF.
    fn read_pipe(rd: OwnedFd) -> Vec<u8> {
        let mut buf = Vec::new();
        std::fs::File::from(rd).read_to_end(&mut buf).unwrap();
        buf
    }

    #[test]
    fn signal_ready_writes_success_byte() {
        let (rd, wr) = make_pipe();
        NotifyPipe::new(wr).signal_ready().unwrap();
        assert_eq!(read_pipe(rd), SUCCESS);
    }

    #[test]
    fn signal_error_writes_protocol() {
        let (rd, wr) = make_pipe();
        let err = DaemonizeError::ForkFailed("boom".into());
        NotifyPipe::new(wr).signal_error(&err);
        assert_eq!(decode(&read_pipe(rd)), decode(&error_bytes(&err)));
    }

    #[test]
    fn signal_unnotified_writes_protocol() {
        let (rd, wr) = make_pipe();
        NotifyPipe::new(wr).signal_unnotified();
        assert_eq!(
            decode(&read_pipe(rd)),
            Outcome::Failure {
                code: UNNOTIFIED_CODE as i32,
                message: "daemon exited without signaling readiness".into(),
            }
        );
    }

    // Covers: R137
    #[test]
    fn drop_without_signal_reports_unnotified_failure() {
        // Safety net: a NotifyPipe that is dropped without any signal_* call
        // (e.g. a panic unwinding past the signalling seam) must report a
        // failure, not let the closed pipe decode as EOF = success.
        let (rd, wr) = make_pipe();
        drop(NotifyPipe::new(wr));
        assert_eq!(
            decode(&read_pipe(rd)),
            Outcome::Failure {
                code: UNNOTIFIED_CODE as i32,
                message: "daemon exited without signaling readiness".into(),
            }
        );
    }

    #[test]
    fn close_writes_nothing() {
        // The intermediate fork processes close their write-end copy silently;
        // the pipe must hit EOF (decoded as success), not the Drop safety net.
        let (rd, wr) = make_pipe();
        NotifyPipe::new(wr).close();
        assert_eq!(read_pipe(rd), Vec::<u8>::new());
    }

    #[test]
    fn drop_after_signal_does_not_double_write() {
        let (rd, wr) = make_pipe();
        NotifyPipe::new(wr).signal_ready().unwrap();
        assert_eq!(read_pipe(rd), SUCCESS);
    }

    #[test]
    fn signal_ready_errors_on_closed_reader() {
        let (rd, wr) = make_pipe();
        drop(rd); // reader gone: write end sees EPIPE
        let result = NotifyPipe::new(wr).signal_ready();
        assert!(result.is_err());
    }

    #[test]
    fn decode_success_byte() {
        assert_eq!(decode(&SUCCESS), Outcome::Success);
    }

    #[test]
    fn decode_eof_is_success() {
        assert_eq!(decode(&[]), Outcome::Success);
    }

    #[test]
    fn error_bytes_round_trip() {
        let err = DaemonizeError::ForkFailed("boom".into());
        let code = err.exit_code();
        assert_eq!(
            decode(&error_bytes(&err)),
            Outcome::Failure {
                code: code as i32,
                message: "fork failed: boom".into(),
            }
        );
    }

    #[test]
    fn application_zero_code_is_not_silently_success() {
        // A caller-chosen code of 0 would collide with the success byte and be
        // decoded as readiness. exit_code() remaps it, so the encoded message
        // is still a real failure.
        let err = DaemonizeError::application(0, "boom");
        match decode(&error_bytes(&err)) {
            Outcome::Failure { code, message } => {
                assert_ne!(code, 0);
                assert_eq!(message, "application error: boom");
            }
            Outcome::Success => panic!("a reported error was decoded as success"),
        }
    }

    #[test]
    fn unnotified_round_trip() {
        assert_eq!(
            decode(&unnotified_bytes()),
            Outcome::Failure {
                code: UNNOTIFIED_CODE as i32,
                message: "daemon exited without signaling readiness".into(),
            }
        );
    }
}
