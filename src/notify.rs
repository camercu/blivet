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

use crate::error::DaemonizeError;

/// Exit-code byte written when a [`DaemonContext`](crate::DaemonContext) is
/// dropped before the daemon signalled readiness.
const UNNOTIFIED_CODE: u8 = 1;

/// Message paired with [`UNNOTIFIED_CODE`].
const UNNOTIFIED_MSG: &[u8] = b"daemon exited without signaling readiness";

/// The single success byte (`0x00`).
pub(crate) const SUCCESS: [u8; 1] = [0x00];

/// Substituted for an exit code of 0 on the failure path, so a reported error
/// can never collide with the [`SUCCESS`] byte. `70` is `EX_SOFTWARE`.
const ZERO_CODE_FALLBACK: u8 = 70;

/// The failure exit code for `err`, guaranteed non-zero.
///
/// A code of 0 (only reachable via a caller-built
/// [`Application`](DaemonizeError::Application) error) would alias the success
/// byte and exit 0, so it is remapped to [`ZERO_CODE_FALLBACK`]. Both the wire
/// message and the daemon's own `_exit` go through here so they always agree.
pub(crate) fn failure_code(err: &DaemonizeError) -> u8 {
    match err.exit_code() {
        0 => ZERO_CODE_FALLBACK,
        c => c,
    }
}

/// Encode a failure message: exit-code byte followed by the error's `Display`.
pub(crate) fn error_bytes(err: &DaemonizeError) -> Vec<u8> {
    encode(failure_code(err), err.to_string().as_bytes())
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
        // decoded as readiness. error_bytes must remap it to a real failure.
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
