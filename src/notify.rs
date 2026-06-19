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
pub(crate) struct NotifyPipe(OwnedFd);

impl NotifyPipe {
    /// Wrap the write end of a notification pipe.
    pub(crate) fn new(fd: OwnedFd) -> Self {
        NotifyPipe(fd)
    }

    /// Borrow the underlying fd, e.g. to exempt it from
    /// [`close_inherited_fds`](crate::steps::close_inherited_fds).
    pub(crate) fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }

    /// Signal readiness: write the success byte and close the pipe.
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if writing to the pipe fails.
    #[must_use = "the parent process blocks until notified; ignoring this Result may leave it waiting"]
    pub(crate) fn signal_ready(self) -> io::Result<()> {
        self.write_all(&SUCCESS)
    }

    /// Signal a daemonization failure: write the error's exit-code byte and
    /// message, then close the pipe. Best-effort — write errors are ignored
    /// because the process is aborting regardless.
    pub(crate) fn signal_error(self, err: &DaemonizeError) {
        let _ = self.write_all(&error_bytes(err));
    }

    /// Signal that the daemon exited without ever calling
    /// [`notify_parent`](crate::DaemonContext::notify_parent). Best-effort.
    pub(crate) fn signal_unnotified(self) {
        let _ = self.write_all(&unnotified_bytes());
    }

    /// Write all bytes to the pipe and flush, consuming and closing it.
    fn write_all(self, bytes: &[u8]) -> io::Result<()> {
        let mut file = io::BufWriter::new(std::fs::File::from(self.0));
        file.write_all(bytes)?;
        file.flush()
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
