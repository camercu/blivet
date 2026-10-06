//! Wire protocol for the parent-readiness notification pipe.
//!
//! After the double fork, the grandchild (the daemon) writes to the pipe; the
//! original parent reads it, and both exit. A message starts with an
//! **exit-code byte**:
//!
//! - `0x00 0x00` ([`SUCCESS`]) — the daemon is ready.
//! - `0x00 0x01` ([`EXEC_PENDING`]) — the daemon is about to `exec`. A
//!   successful `exec` closes the `O_CLOEXEC` write end, so EOF after this
//!   marker is success; a failed one is followed by an error message.
//! - non-zero `code`, the message's length as two big-endian bytes, then the
//!   UTF-8 message — failure; the parent prints `message` to stderr and exits
//!   with `code` (R160).
//! - EOF with no marker — the daemon ended without notifying (a signal killed
//!   it, or it exited without dropping its context): failure (R150).
//!
//! Both two-byte messages go out in one `write` well under `PIPE_BUF`, so the
//! reader never waits for the second byte.
//!
//! Centralizing the byte layout here keeps the three writers (success, error,
//! drop-without-notify) and the single reader from drifting out of sync.

use std::io::{self, Read, Write};
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

    /// Signal readiness: write [`SUCCESS`] and close the pipe.
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if writing to the pipe fails.
    #[must_use = "the parent process blocks until notified; ignoring this Result may leave it waiting"]
    pub(crate) fn signal_ready(mut self) -> io::Result<()> {
        self.write_all(&SUCCESS)
    }

    /// Say the daemon is about to `exec`, keeping the pipe open: a successful
    /// `exec` closes it (EOF reads as success), and a failed one reports its
    /// error through [`signal_error`](Self::signal_error).
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if writing to the pipe fails.
    pub(crate) fn signal_exec_pending(&mut self) -> io::Result<()> {
        let fd = self
            .0
            .as_ref()
            .expect("exec marker written after the pipe was consumed");
        // Through the borrowed fd: a `dup` to get a `File` would need a free
        // descriptor, and a daemon at its limit must still be able to exec.
        BorrowedWriter(fd.as_fd()).write_all(&EXEC_PENDING)
    }

    /// Signal a daemonization failure: write the error's exit-code byte and
    /// message, then close the pipe. If the write fails, most likely because
    /// the launcher is gone, the message goes to stderr instead, so it is not
    /// lost with the pipe (R161). The process is aborting either way.
    pub(crate) fn signal_error(mut self, err: &DaemonizeError) {
        // The process exits or returns an error next, so the caller's SIGPIPE
        // disposition (R127) no longer matters, and with SIG_DFL a gone
        // launcher would kill it before it could print.
        crate::signals::ignore_sigpipe();
        if self.write_all(&error_bytes(err)).is_err() {
            print_on_stderr(err);
        }
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

/// `Write` over a descriptor it does not own, so `write_all` brings its
/// retry on `EINTR` and on a short write.
struct BorrowedWriter<'a>(BorrowedFd<'a>);

impl Write for BorrowedWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(nix::unistd::write(self.0, buf)?)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Print `message` on stderr, best-effort.
///
/// `eprintln!` panics when the write fails (a pipe nobody reads, a full
/// disk), and on a library error path that panic unwinds into the caller's
/// code, inside the daemon, instead of exiting with the error's code.
pub(crate) fn print_on_stderr(message: impl std::fmt::Display) {
    let _ = writeln!(io::stderr(), "{message}");
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

/// Message for a pipe that closed with no message and no exec marker.
const EOF_UNNOTIFIED_MSG: &str =
    "daemon ended without signaling readiness (killed by a signal, or exited without dropping its DaemonContext)";

/// Readiness: the daemon is up.
pub(crate) const SUCCESS: [u8; 2] = [0x00, 0x00];

/// The daemon is about to `exec`; EOF after this is success.
pub(crate) const EXEC_PENDING: [u8; 2] = [0x00, 0x01];

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

/// A failure on the wire: the code byte, the message's length as two
/// big-endian bytes, then the message (R160). The length lets the launcher
/// stop at the message's end instead of waiting for EOF.
fn encode(code: u8, msg: &[u8]) -> Vec<u8> {
    let msg = &msg[..msg.len().min(u16::MAX as usize)];
    let mut buf = Vec::with_capacity(3 + msg.len());
    buf.push(code);
    buf.extend_from_slice(&(msg.len() as u16).to_be_bytes());
    buf.extend_from_slice(msg);
    buf
}

/// The decoded result of a message read from the notification pipe.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Daemon is ready, or exec'd after the exec marker.
    Success,
    /// Daemon failed; report `message` to stderr and exit with `code`.
    Failure { code: i32, message: String },
}

/// Read the daemon's message from the pipe and decode it.
///
/// Returns on [`SUCCESS`] without reading on. A process the daemon forked
/// before notifying holds a copy of the write end, so the pipe reaches EOF only
/// when that process exits too: waiting for EOF would hold the launcher until
/// then. A failure reads only its length-framed message (R160), for the same
/// reason. After [`EXEC_PENDING`] the read waits for the exec: a successful
/// one closes the pipe (EOF), a failed one sends its error.
pub(crate) fn read_outcome(pipe: &mut impl io::Read) -> Outcome {
    let mut buf = Vec::new();
    let Some(first) = read_byte(pipe) else {
        return decode(&buf);
    };
    buf.push(first);
    if first == 0 {
        // Sent in the same write as the first byte, so it is already here.
        let Some(second) = read_byte(pipe) else {
            return decode(&buf);
        };
        buf.push(second);
        if buf != EXEC_PENDING {
            return decode(&buf);
        }
        // After the exec marker: EOF when the exec closes the pipe, or the
        // exec's error.
        let Some(code) = read_byte(pipe) else {
            return decode(&buf);
        };
        buf.push(code);
    }
    // An error message: its length, then that many bytes, and no further.
    let mut len = Vec::new();
    let _ = pipe.by_ref().take(2).read_to_end(&mut len);
    buf.extend_from_slice(&len);
    if let Ok(len) = <[u8; 2]>::try_from(len.as_slice()) {
        let _ = pipe
            .by_ref()
            .take(u64::from(u16::from_be_bytes(len)))
            .read_to_end(&mut buf);
    }
    decode(&buf)
}

/// One byte from the pipe, or `None` at EOF or on an error. A read a signal
/// interrupts is retried (R158): `read` returns `EINTR` then, not data.
fn read_byte(pipe: &mut impl io::Read) -> Option<u8> {
    let mut byte = [0u8; 1];
    loop {
        match pipe.read(&mut byte) {
            Ok(1) => return Some(byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            _ => return None,
        }
    }
}

/// Decode a whole message read from the notification pipe.
pub(crate) fn decode(buf: &[u8]) -> Outcome {
    if buf == SUCCESS {
        return Outcome::Success;
    }
    if let Some(rest) = buf.strip_prefix(&EXEC_PENDING[..]) {
        // The exec marker, then EOF (the exec closed the pipe) or its error.
        return if rest.is_empty() {
            Outcome::Success
        } else {
            decode(rest)
        };
    }
    match buf.first() {
        None => Outcome::Failure {
            code: UNNOTIFIED_CODE as i32,
            message: EOF_UNNOTIFIED_MSG.into(),
        },
        // Only SUCCESS and EXEC_PENDING start with 0x00 (R159).
        Some(&0x00) => Outcome::Failure {
            code: UNNOTIFIED_CODE as i32,
            message: format!(
                "daemon sent the launcher a malformed message: {:02x?}",
                &buf[..buf.len().min(8)]
            ),
        },
        Some(&code) => {
            let framed = buf
                .get(1..3)
                .map(|len| u16::from_be_bytes([len[0], len[1]]) as usize);
            let message = match framed {
                Some(len) => &buf[3..buf.len().min(3 + len)],
                None => &buf[1..],
            };
            Outcome::Failure {
                code: code as i32,
                message: String::from_utf8_lossy(message).into_owned(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{make_pipe, read_pipe};

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
    fn drop_with_closed_reader_does_not_panic() {
        // The Drop safety net runs during unwinds; a failing pipe write there
        // (reader already gone -> EPIPE) must stay silent, or the safety net
        // would turn one panic into an abort.
        let (rd, wr) = make_pipe();
        drop(rd);
        drop(NotifyPipe::new(wr));
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

    /// A pipe that has delivered `.0` and whose write end another process still
    /// holds open, as one forked before `notify_parent` does: reading past the
    /// delivered bytes would block the launcher.
    struct StillOpen(std::collections::VecDeque<u8>);

    impl io::Read for StillOpen {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            assert!(
                !self.0.is_empty(),
                "read past the message: the launcher would block"
            );
            let n = buf.len().min(self.0.len());
            for slot in &mut buf[..n] {
                *slot = self.0.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    /// A pipe read that a signal interrupts before each byte, as a SIGCHLD
    /// arriving in the launcher would.
    struct Interrupted(std::collections::VecDeque<u8>, bool);

    impl io::Read for Interrupted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.1 = !self.1;
            if self.1 {
                return Err(io::ErrorKind::Interrupted.into());
            }
            match self.0.pop_front() {
                Some(byte) => {
                    buf[0] = byte;
                    Ok(1)
                }
                None => Ok(0),
            }
        }
    }

    // Covers: R158
    #[test]
    fn read_outcome_retries_a_read_a_signal_interrupts() {
        let mut ready = Interrupted(SUCCESS.into_iter().collect(), false);
        assert_eq!(read_outcome(&mut ready), Outcome::Success);

        let err = DaemonizeError::ForkFailed("boom".into());
        let mut failed = Interrupted(error_bytes(&err).into_iter().collect(), false);
        assert_eq!(read_outcome(&mut failed), decode(&error_bytes(&err)));
    }

    /// A pipe whose read fails for good, as on `EBADF`.
    struct Broken;

    impl io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::Other.into())
        }
    }

    // Covers: R158
    #[test]
    fn read_outcome_gives_up_on_a_read_error_other_than_eintr() {
        assert_eq!(read_outcome(&mut Broken), decode(&[]));
    }

    #[test]
    fn read_outcome_stops_at_the_success_byte() {
        let mut pipe = StillOpen(SUCCESS.into_iter().collect());
        assert_eq!(read_outcome(&mut pipe), Outcome::Success);
    }

    // Covers: R160
    #[test]
    fn read_outcome_stops_at_the_end_of_an_error_message() {
        let err = DaemonizeError::ForkFailed("boom".into());
        let mut pipe = StillOpen(error_bytes(&err).into_iter().collect());
        assert_eq!(
            read_outcome(&mut pipe),
            Outcome::Failure {
                code: err.exit_code() as i32,
                message: "fork failed: boom".into(),
            }
        );
    }

    // Covers: R160
    #[test]
    fn decode_takes_the_message_its_length_names() {
        let err = DaemonizeError::ForkFailed("boom".into());
        let mut bytes = error_bytes(&err);
        bytes.extend_from_slice(b" and more");
        assert_eq!(decode(&bytes), decode(&error_bytes(&err)));
    }

    // Covers: R160
    #[test]
    fn read_outcome_stops_at_the_end_of_an_exec_error() {
        let err = DaemonizeError::ExecFailed("exec /x: E2BIG".into());
        let mut bytes = EXEC_PENDING.to_vec();
        bytes.extend(error_bytes(&err));
        let mut pipe = StillOpen(bytes.into_iter().collect());
        assert_eq!(read_outcome(&mut pipe), decode(&error_bytes(&err)));
    }

    // Covers: R159
    #[test]
    fn a_malformed_header_is_a_failure_that_says_so() {
        for bytes in [&[0x00][..], &[0x00, 0x07][..], &[0x00, 0x07, b'x'][..]] {
            match read_outcome(&mut &bytes[..]) {
                Outcome::Failure { code, message } => {
                    assert_eq!(code, UNNOTIFIED_CODE as i32, "{bytes:?}");
                    assert!(message.contains("malformed"), "{bytes:?}: {message}");
                }
                Outcome::Success => panic!("{bytes:?} read as readiness"),
            }
        }
    }

    // Covers: R150
    #[test]
    fn eof_without_the_exec_marker_is_failure() {
        assert_eq!(
            decode(&[]),
            Outcome::Failure {
                code: UNNOTIFIED_CODE as i32,
                message: EOF_UNNOTIFIED_MSG.into(),
            }
        );
    }

    // Covers: R150
    #[test]
    fn eof_after_the_exec_marker_is_success() {
        assert_eq!(read_outcome(&mut &EXEC_PENDING[..]), Outcome::Success);
    }

    // Covers: R150
    #[test]
    fn an_error_after_the_exec_marker_is_that_error() {
        let err = DaemonizeError::ExecFailed("exec /x: E2BIG".into());
        let mut bytes = EXEC_PENDING.to_vec();
        bytes.extend(error_bytes(&err));
        assert_eq!(read_outcome(&mut &bytes[..]), decode(&error_bytes(&err)));
    }

    // Covers: R150
    #[test]
    fn signal_exec_pending_leaves_the_pipe_open_for_an_exec_error() {
        let (rd, wr) = make_pipe();
        let err = DaemonizeError::ProgramNotFound("exec /x: ENOENT".into());
        let mut pipe = NotifyPipe::new(wr);
        pipe.signal_exec_pending().unwrap();
        pipe.signal_error(&err);
        assert_eq!(
            read_outcome(&mut std::fs::File::from(rd)),
            decode(&error_bytes(&err))
        );
    }

    // Covers: R150
    #[test]
    fn the_exec_marker_needs_no_free_descriptor() {
        crate::test_support::run_in_subprocess(
            "notify::tests::the_exec_marker_needs_no_free_descriptor_subprocess",
        );
    }

    /// A daemon at its descriptor limit must still be able to `exec`.
    #[test]
    #[ignore]
    fn the_exec_marker_needs_no_free_descriptor_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let (rd, wr) = make_pipe();
        let mut pipe = NotifyPipe::new(wr);
        let held = crate::test_support::fill_fd_table(0);
        let written = pipe.signal_exec_pending();
        drop(held);
        written.unwrap();
        pipe.close();
        assert_eq!(read_pipe(rd), EXEC_PENDING);
    }

    // Covers: R161
    #[test]
    fn an_error_report_with_nowhere_to_go_does_not_panic() {
        crate::test_support::run_in_subprocess(
            "notify::tests::an_error_report_with_nowhere_to_go_does_not_panic_subprocess",
        );
    }

    /// The launcher is gone and stderr is a pipe nobody reads (or a full
    /// disk): the report is lost, but a panic here would unwind into the
    /// caller's code inside the daemon instead of exiting.
    #[test]
    #[ignore]
    fn an_error_report_with_nowhere_to_go_does_not_panic_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let (rd, wr) = make_pipe();
        drop(rd);
        let (err_rd, err_wr) = make_pipe();
        drop(err_rd);
        #[allow(clippy::disallowed_methods)] // the test's own process
        nix::unistd::dup2_stderr(err_wr).unwrap();

        let reported = std::panic::catch_unwind(|| {
            NotifyPipe::new(wr).signal_error(&DaemonizeError::ForkFailed("boom".into()));
        });

        assert!(reported.is_ok());
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
        // A caller-chosen code of 0 would collide with the readiness header and be
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
