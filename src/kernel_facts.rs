//! Probes of the kernel behaviour the code assumes on every platform.
//!
//! `capability_probes` checks the claims in `build.rs`, which differ by
//! platform. These are the facts that do not: each test asserts one, and names
//! the code that depends on it. The day a platform changes one, its probe goes
//! red and says what to revisit.
//!
//! They call the system directly, not through the code they protect, so a bug
//! there cannot make its own probe pass.

#![allow(unsafe_code)]
#![allow(clippy::disallowed_methods)] // probes make their own socket pairs

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crate::test_support::{is_subprocess, run_in_subprocess};

/// A connected `AF_UNIX` stream pair, as `notify::channel` makes one.
fn socket_pair() -> (OwnedFd, OwnedFd) {
    let mut fds = [0; 2];
    // SAFETY: on success socketpair writes two new descriptors into `fds`.
    let ret = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    nix::errno::Errno::result(ret).unwrap();
    // SAFETY: each descriptor is new, open, and owned by nothing else.
    let [a, b] = fds.map(|fd| unsafe { OwnedFd::from_raw_fd(fd) });
    (a, b)
}

/// `send` with `flags`, as an `io::Result` of the byte count.
fn send(fd: &OwnedFd, buf: &[u8], flags: libc::c_int) -> std::io::Result<usize> {
    // SAFETY: `buf` is valid for `buf.len()` bytes, and `fd` is open.
    let sent = unsafe { libc::send(fd.as_raw_fd(), buf.as_ptr().cast(), buf.len(), flags) };
    Ok(nix::errno::Errno::result(sent)?.unsigned_abs())
}

/// One `recv` of up to `buf.len()` bytes that does not wait.
fn recv_now(fd: &OwnedFd, buf: &mut [u8]) -> std::io::Result<usize> {
    // SAFETY: `buf` is writable for `buf.len()` bytes, and `fd` is open.
    let got = unsafe {
        libc::recv(
            fd.as_raw_fd(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            libc::MSG_DONTWAIT,
        )
    };
    Ok(nix::errno::Errno::result(got)?.unsigned_abs())
}

// Depends: notify::ChannelWriter (R165).
#[test]
fn a_send_to_a_closed_peer_fails_with_epipe_and_raises_no_sigpipe() {
    run_in_subprocess(
        "kernel_facts::a_send_to_a_closed_peer_fails_with_epipe_and_raises_no_sigpipe_subprocess",
    );
}

/// With `SIGPIPE` at its default, a raised one would kill this subprocess and
/// fail the wrapper.
#[test]
#[ignore]
fn a_send_to_a_closed_peer_fails_with_epipe_and_raises_no_sigpipe_subprocess() {
    if !is_subprocess() {
        return;
    }
    // SAFETY: SIG_DFL installs no handler.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let (ours, peer) = socket_pair();
    drop(peer);

    let sent = send(&ours, b"x", libc::MSG_NOSIGNAL);

    assert_eq!(sent.map_err(|e| e.raw_os_error()), Err(Some(libc::EPIPE)));
}

// Depends: notify::channel.
#[test]
fn shutting_down_reads_on_one_end_then_writes_on_the_other_succeeds() {
    let (launcher, daemon) = socket_pair();
    // SAFETY: both descriptors are open.
    let rd = unsafe { libc::shutdown(daemon.as_raw_fd(), libc::SHUT_RD) };
    // SAFETY: as above.
    let wr = unsafe { libc::shutdown(launcher.as_raw_fd(), libc::SHUT_WR) };
    assert_eq!((rd, wr), (0, 0), "{}", std::io::Error::last_os_error());
}

// Depends: notify::read_outcome, and the protocol section of docs/SPEC.md.
#[test]
fn a_two_byte_send_arrives_whole() {
    let (rd, wr) = socket_pair();
    send(&wr, &[0x00, 0x01], 0).unwrap();
    let (mut first, mut second) = ([0; 1], [0; 1]);

    let got = (
        recv_now(&rd, &mut first).ok(),
        recv_now(&rd, &mut second).ok(),
    );

    assert_eq!(got, (Some(1), Some(1)));
}

// Depends: steps::above_stdio (R152, R107).
#[test]
fn f_dupfd_cloexec_lands_at_or_above_its_floor_close_on_exec() {
    let (fd, _peer) = socket_pair();
    let floor = 10;
    // SAFETY: `fd` is open; F_DUPFD_CLOEXEC returns a new descriptor.
    let dup = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, floor) };
    let dup = nix::errno::Errno::result(dup).unwrap();
    // SAFETY: the descriptor is new and owned by nothing else.
    let dup = unsafe { OwnedFd::from_raw_fd(dup) };
    // SAFETY: `dup` is open.
    let flags = unsafe { libc::fcntl(dup.as_raw_fd(), libc::F_GETFD) };

    assert_eq!(
        (dup.as_raw_fd() >= floor, flags & libc::FD_CLOEXEC),
        (true, libc::FD_CLOEXEC)
    );
}

// Depends: the signal-disposition undo token, signals::reset_signal_dispositions (R164).
#[test]
fn the_replaced_action_sigaction_reports_restores_the_original_exactly() {
    run_in_subprocess(
        "kernel_facts::the_replaced_action_sigaction_reports_restores_the_original_exactly_subprocess",
    );
}

extern "C" fn handler(_: libc::c_int) {}

#[test]
#[ignore]
fn the_replaced_action_sigaction_reports_restores_the_original_exactly_subprocess() {
    if !is_subprocess() {
        return;
    }
    let read = || {
        let mut now = crate::signals::zeroed_sigaction();
        // SAFETY: a null new action only reads the disposition.
        let ret = unsafe { libc::sigaction(libc::SIGUSR1, std::ptr::null(), &mut now) };
        nix::errno::Errno::result(ret).unwrap();
        (now.sa_sigaction, now.sa_flags)
    };
    let mut original = crate::signals::zeroed_sigaction();
    original.sa_sigaction = handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
    original.sa_flags = libc::SA_RESTART | libc::SA_NODEFER;
    // SAFETY: the handler does nothing, so it is async-signal-safe.
    unsafe { libc::sigaction(libc::SIGUSR1, &original, std::ptr::null_mut()) };
    let before = read();
    let mut dfl = crate::signals::zeroed_sigaction();
    dfl.sa_sigaction = libc::SIG_DFL;
    let mut replaced = crate::signals::zeroed_sigaction();
    // SAFETY: `dfl` is SIG_DFL, and `replaced` is a valid sigaction to fill.
    unsafe { libc::sigaction(libc::SIGUSR1, &dfl, &mut replaced) };

    // SAFETY: `replaced` is what sigaction itself reported for SIGUSR1.
    unsafe { libc::sigaction(libc::SIGUSR1, &replaced, std::ptr::null_mut()) };

    assert_eq!(read(), before);
}
