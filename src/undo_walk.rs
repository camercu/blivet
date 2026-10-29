//! The undo walk: fails a foreground start after each undo token in turn,
//! just before `init`, and once more just before the identity switch, and
//! checks after each failure that the process is as it was before the start
//! (R164).
//!
//! The walk holds no list of steps. A step that changes the snapshot's state
//! without returning a token leaves that change behind at every failure after
//! it, and the comparison fails.

use crate::forker::null_forker::NullForker;
use crate::steps::failpoints;
use crate::test_support::{ProcessSnapshot, isolated};
use crate::{DaemonConfig, DaemonizeError};

extern "C" fn caller_handler(_: libc::c_int) {}

/// Gives the caller state each step would visibly change: a handler, a blocked
/// signal, a umask the config does not ask for.
#[allow(unsafe_code)]
fn caller_state() {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
    let handler = SigAction::new(
        SigHandler::Handler(caller_handler),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    // SAFETY: the handler does nothing, so it is async-signal-safe.
    unsafe { sigaction(Signal::SIGUSR1, &handler) }.unwrap();
    let mut blocked = SigSet::empty();
    blocked.add(Signal::SIGUSR2);
    blocked.thread_block().unwrap();
    nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(
        crate::test_support::CALLER_UMASK,
    ));
    #[cfg(any(target_os = "linux", target_os = "android"))]
    realtime_caller_state();
}

/// The same for the real-time signals above 31, which a snapshot that read
/// only the classic ones would miss.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[allow(unsafe_code)]
fn realtime_caller_state() {
    let handled = libc::SIGRTMIN();
    let blocked = libc::SIGRTMIN() + 1;
    let handler = caller_handler as extern "C" fn(libc::c_int);
    // SAFETY: all-zero bytes are a valid `sigaction` and `sigset_t`.
    let (mut sa, mut set): (libc::sigaction, libc::sigset_t) = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = handler as libc::sighandler_t;
    // SAFETY: `sa.sa_mask` is a valid set to empty.
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
    // SAFETY: the handler does nothing, so it is async-signal-safe.
    let ret = unsafe { libc::sigaction(handled, &sa, std::ptr::null_mut()) };
    assert_eq!(ret, 0, "sigaction");
    // SAFETY: `set` is a valid set to fill.
    unsafe { libc::sigemptyset(&mut set) };
    // SAFETY: `set` is initialised and `blocked` is a real-time signal.
    unsafe { libc::sigaddset(&mut set, blocked) };
    // SAFETY: `set` is initialised; a null old set reads nothing back.
    let ret = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
    assert_eq!(ret, 0, "pthread_sigmask");
    let (dispositions, mask) = crate::test_support::signal_state();
    assert!(
        mask.contains(&blocked)
            && dispositions
                .iter()
                .any(|d| d.0 == handled && d.1 == handler as usize),
        "the snapshot misses the real-time signals"
    );
}

/// Walks `config`'s foreground start; `pidfile` is the one it writes.
fn walk(config: &DaemonConfig, pidfile: &std::path::Path) {
    failpoints::BEFORE_SWITCH_FAILS.set();
    let before = ProcessSnapshot::take(pidfile);
    for n in 1.. {
        failpoints::fail_at_checkpoint(n);
        let mut init_ran = false;
        // SAFETY: NullForker does not fork.
        #[allow(unsafe_code)]
        let result = unsafe {
            crate::sequence::daemonize_inner_with(
                config,
                &mut NullForker::new(vec![], Ok(())),
                || {
                    init_ran = true;
                    Ok(())
                },
            )
        };
        let Err(e) = result else {
            panic!("the start succeeded with failpoints set")
        };
        let last = match &e {
            DaemonizeError::SystemError(m) if m == failpoints::CHECKPOINT_FAILURE => false,
            DaemonizeError::SystemError(m) if m == failpoints::SWITCH_FAILURE => true,
            other => panic!("failure {n} is not a failpoint's: {other:?}"),
        };
        let after = ProcessSnapshot::take(pidfile);
        // From init on, the env stays as step 11 set it (R164).
        let (expected, after) = if init_ran {
            (before.clone().without_env(), after.without_env())
        } else {
            (before.clone(), after)
        };
        assert_eq!(after, expected, "state differs after failure {n}");
        if last {
            assert!(n > 1, "the walk failed no token");
            return;
        }
    }
}

/// The config every step with a token acts on: umask, cwd, the stdio slots,
/// signals, env, and with `user` the groups. The fd sweep runs too, so a
/// token whose descriptor it does not spare loses what it saved.
fn full_config(dir: &std::path::Path) -> DaemonConfig {
    let mut config = DaemonConfig::new();
    config
        .foreground(true)
        .close_fds(true)
        .umask(0o077)
        .chdir(dir)
        .pidfile(dir.join("app.pid"))
        .stdout(dir.join("out.log"))
        .stderr(dir.join("err.log"))
        .env("BLIVET_WALK_SET", "daemon")
        .env("PATH", "/daemon/bin");
    config
}

// Covers: R164
#[test]
fn a_foreground_start_failing_anywhere_leaves_the_process_as_it_was() {
    if !isolated() {
        return;
    }
    caller_state();
    let dir = tempfile::tempdir().unwrap();
    let config = full_config(dir.path());
    walk(&config, &dir.path().join("app.pid"));
}

// Covers: R164
#[test]
#[ignore = "needs root: the privileged tier runs it"]
fn root_failing_anywhere_before_the_switch_leaves_the_process_as_it_was() {
    if !isolated() {
        return;
    }
    caller_state();
    crate::sys::raw_setgroups(&[0, 4242, 4343]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = full_config(dir.path());
    config.user("nobody");
    walk(&config, &dir.path().join("app.pid"));
}
