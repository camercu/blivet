use std::fmt;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

use nix::fcntl::Flock;

use crate::error::DaemonizeError;

/// Context returned by a successful daemonization.
///
/// Holds the lockfile (if configured) and the notification pipe write end.
/// Dropping this without calling [`notify_parent`](DaemonContext::notify_parent)
/// writes a failure message to the notification pipe, causing the parent to
/// exit non-zero.
///
/// The lock is released when this value is dropped.
#[non_exhaustive]
pub struct DaemonContext {
    lockfile: Option<Flock<OwnedFd>>,
    notify_pipe: Option<OwnedFd>,
}

impl fmt::Debug for DaemonContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonContext")
            .field(
                "lockfile",
                if self.lockfile.is_some() {
                    &"present"
                } else {
                    &"absent"
                },
            )
            .field(
                "notify_pipe",
                if self.notify_pipe.is_some() {
                    &"present"
                } else {
                    &"absent"
                },
            )
            .finish()
    }
}

impl DaemonContext {
    pub(crate) fn new(lockfile: Option<Flock<OwnedFd>>, notify_pipe: Option<OwnedFd>) -> Self {
        Self {
            lockfile,
            notify_pipe,
        }
    }

    /// Returns a borrowed reference to the lockfile fd, or `None` if no
    /// lockfile was configured.
    ///
    /// The returned fd has `O_CLOEXEC` set. If you intend to `exec` and want
    /// the lock to survive, clear `CLOEXEC` before calling `exec`.
    pub fn lockfile_fd(&self) -> Option<BorrowedFd<'_>> {
        self.lockfile.as_ref().map(|flock| flock.as_fd())
    }

    /// Signals the parent that the daemon is ready.
    ///
    /// Writes a success byte (`0x00`) to the notification pipe and closes it.
    /// The parent reads this and exits 0.
    ///
    /// After this call, subsequent calls are no-ops (the pipe is consumed).
    ///
    /// # Errors
    ///
    /// Returns `io::Error` if writing to the pipe fails.
    pub fn notify_parent(&mut self) -> Result<(), io::Error> {
        if let Some(fd) = self.notify_pipe.take() {
            let mut file = io::BufWriter::new(fd_to_file(fd));
            file.write_all(&[0x00])?;
            file.flush()?;
        }
        Ok(())
    }

    /// Reports an error to the parent process and exits.
    ///
    /// Writes the error's exit code byte followed by the `Display` message to
    /// the notification pipe, then calls `_exit()`. The parent reads this,
    /// prints the message to stderr, and exits with the code.
    pub fn report_error(&mut self, err: &DaemonizeError) -> ! {
        if let Some(fd) = self.notify_pipe.take() {
            let raw_fd = fd.as_raw_fd();
            let mut file = io::BufWriter::new(fd_to_file(fd));
            let msg = err.to_string();
            let code = err.exit_code();
            let mut buf = Vec::with_capacity(1 + msg.len());
            buf.push(code);
            buf.extend_from_slice(msg.as_bytes());
            let _ = file.write_all(&buf);
            let _ = file.flush();
            drop(file);
            let _ = nix::unistd::close(raw_fd);
        }
        std::process::exit(err.exit_code() as i32)
    }
}

impl Drop for DaemonContext {
    fn drop(&mut self) {
        if let Some(fd) = self.notify_pipe.take() {
            let mut file = io::BufWriter::new(fd_to_file(fd));
            let msg = b"daemon exited without signaling readiness";
            let mut buf = Vec::with_capacity(1 + msg.len());
            buf.push(1u8);
            buf.extend_from_slice(msg);
            let _ = file.write_all(&buf);
            let _ = file.flush();
        }
    }
}

fn fd_to_file(fd: OwnedFd) -> std::fs::File {
    std::fs::File::from(fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn make_pipe() -> (OwnedFd, OwnedFd) {
        nix::unistd::pipe().unwrap()
    }

    fn read_pipe(rd: OwnedFd) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut file = std::fs::File::from(rd);
        file.read_to_end(&mut buf).unwrap();
        buf
    }

    #[test]
    fn notify_parent_writes_success_byte() {
        let (rd, wr) = make_pipe();
        let mut ctx = DaemonContext::new(None, Some(wr));
        ctx.notify_parent().unwrap();
        assert_eq!(read_pipe(rd), vec![0x00]);
    }

    #[test]
    fn notify_parent_idempotent() {
        let (_rd, wr) = make_pipe();
        let mut ctx = DaemonContext::new(None, Some(wr));
        ctx.notify_parent().unwrap();
        ctx.notify_parent().unwrap();
    }

    #[test]
    fn drop_writes_failure_when_not_notified() {
        let (rd, wr) = make_pipe();
        {
            let _ctx = DaemonContext::new(None, Some(wr));
        }

        let buf = read_pipe(rd);
        assert_eq!(buf[0], 1u8);
        assert_eq!(
            std::str::from_utf8(&buf[1..]).unwrap(),
            "daemon exited without signaling readiness"
        );
    }

    #[test]
    fn drop_no_write_after_notify() {
        let (rd, wr) = make_pipe();
        {
            let mut ctx = DaemonContext::new(None, Some(wr));
            ctx.notify_parent().unwrap();
        }
        assert_eq!(read_pipe(rd), vec![0x00]);
    }

    #[test]
    fn debug_format() {
        let ctx = DaemonContext::new(None, None);
        let debug = format!("{:?}", ctx);
        assert!(debug.contains("absent"));
    }
}
