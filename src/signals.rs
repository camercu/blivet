//! Signal handling: the step-9 reset of every disposition, the pidfile
//! cleanup handler behind `DaemonContext::cleanup_on_signals`, and the
//! `SIGPIPE` restore before an exec.
//!
//! The `sigaction` and `signal` FFI this needs is allowed at module level,
//! next to the code whose invariants it upholds.

#![allow(unsafe_code)]

use crate::steps::{Restore, Rollback, Undo};

/// Reset signal dispositions from 1 through the signal ceiling to SIG_DFL.
///
/// On Linux, iterates standard signals (1..32) then real-time signals
/// (SIGRTMIN..=SIGRTMAX), skipping the NPTL-reserved range (32..SIGRTMIN).
/// On other platforms, iterates 1..=64 and silently skips EINVAL.
///
/// SIGKILL and SIGSTOP are always skipped (cannot be caught/reset).
///
/// SIGPIPE is also skipped, preserving the caller's disposition (R127): the
/// Rust runtime installs SIG_IGN so writes to a closed pipe or socket return
/// `EPIPE` instead of killing the process, and resetting it would revoke
/// that for every write the daemon makes. `DaemonContext::exec` sets SIG_DFL
/// just before `execvp` (R128), so exec'd programs still start with the
/// conventional disposition.
///
/// Returns [`SystemError`](crate::error::DaemonizeError::SystemError) on a
/// non-`EINVAL` `sigaction` failure (POSIX documents only `EINVAL` for these
/// arguments, so this is a should-not-happen guard) so the caller reports it
/// to the parent rather than panicking. `EINVAL` — an uncatchable or invalid
/// signal number — is skipped silently.
///
/// The token puts back each disposition it replaced, latest first (R164). A
/// failure part way puts back those already replaced before it returns.
pub(crate) fn reset_signal_dispositions(
    rollback: Rollback,
) -> Result<Undo, crate::error::DaemonizeError> {
    #[cfg(test)]
    crate::test_support::assert_isolated("reset_signal_dispositions");
    let mut saved = Dispositions(Vec::new());
    let signals = signal_range();
    for sig in signals {
        if matches!(sig, libc::SIGKILL | libc::SIGSTOP | libc::SIGPIPE) {
            continue;
        }

        let mut was = zeroed_sigaction();
        match reset(sig, &mut was) {
            Ok(()) => {
                if let Rollback::PutBack = rollback {
                    saved.0.push(Disposition { sig, was });
                }
            }
            // EINVAL: signal cannot be caught, skip
            Err(err) if err.raw_os_error() == Some(libc::EINVAL) => {}
            Err(err) => {
                drop(Undo::new(saved));
                return Err(crate::error::DaemonizeError::SystemError(format!(
                    "sigaction({sig}): {err}"
                )));
            }
        }
    }
    Ok(Undo::saving(rollback, || saved))
}

/// Sets `sig` to SIG_DFL, reporting what it replaced in `was`.
fn reset(sig: i32, was: &mut libc::sigaction) -> std::io::Result<()> {
    #[cfg(test)]
    if sig == libc::SIGTERM && crate::steps::failpoints::SIGACTION_FAILS.injected() {
        return Err(std::io::Error::other("injected failure"));
    }
    let sa = new_sigaction(libc::SIG_DFL, 0);
    // SAFETY: `sa` is an initialised SIG_DFL action, and `was` is a valid
    // sigaction for the kernel to fill.
    let ret = unsafe { libc::sigaction(sig, &sa, was) };
    nix::errno::Errno::result(ret)?;
    Ok(())
}

/// A signal's disposition as step 9 found it.
struct Disposition {
    sig: i32,
    was: libc::sigaction,
}

/// The dispositions step 9 replaced, in the order it replaced them.
struct Dispositions(Vec<Disposition>);

impl Restore for Dispositions {
    fn restore(self: Box<Self>) {
        for Disposition { sig, was } in self.0.into_iter().rev() {
            // SAFETY: `was` is a disposition sigaction itself reported for
            // `sig`.
            unsafe { libc::sigaction(sig, &was, std::ptr::null_mut()) };
        }
    }
}

/// A `sigaction` with every field zero, for the kernel to fill.
pub(crate) fn zeroed_sigaction() -> libc::sigaction {
    // SAFETY: `sigaction` is a C struct of integers, a handler word and a
    // signal set, and all-zero bytes are a valid value of each.
    unsafe { std::mem::zeroed() }
}

/// A `sigaction` that installs `handler` with `flags` and an empty mask.
fn new_sigaction(handler: libc::sighandler_t, flags: libc::c_int) -> libc::sigaction {
    let mut sa = zeroed_sigaction();
    sa.sa_sigaction = handler;
    // SAFETY: `sa_mask` is a valid signal set owned by `sa`.
    unsafe { libc::sigemptyset(&mut sa.sa_mask) };
    sa.sa_flags = flags;
    sa
}

/// `sig`'s current disposition, read without changing it.
pub(crate) fn disposition(sig: i32) -> std::io::Result<libc::sigaction> {
    let mut now = zeroed_sigaction();
    // SAFETY: a null new action only reads the disposition into `now`.
    let ret = unsafe { libc::sigaction(sig, std::ptr::null(), &mut now) };
    nix::errno::Errno::result(ret)?;
    Ok(now)
}

/// Returns the range of signal numbers to reset.
///
/// Where libc reserves real-time signals, returns the standard signals (1..32)
/// chained with `SIGRTMIN..=SIGRTMAX`, so the reserved range between the two
/// (the NPTL pair at 32-33 on glibc) is skipped rather than reset.
///
/// Elsewhere, returns 1..=64 (EINVAL skips invalid ones). That covers the
/// standard signals everywhere, and also the real-time ones on a platform
/// whose range falls below 64 — NetBSD's 33-63 does, so its real-time
/// dispositions are reset rather than left alone. See the capability's own doc
/// in `build.rs` for why that is acceptable today and what would change it.
fn signal_range() -> Vec<i32> {
    #[cfg(blivet_rt_signals_reserved)]
    {
        let rtmin = libc::SIGRTMIN();
        let rtmax = libc::SIGRTMAX();
        (1..32).chain(rtmin..=rtmax).collect()
    }
    #[cfg(not(blivet_rt_signals_reserved))]
    {
        // No real-time signals to preserve; 31 standard signals, but iterate up
        // to 64 to be safe (EINVAL will skip invalid ones).
        (1..=64).collect()
    }
}

/// Path to unlink from inside the async-signal-safe cleanup handler.
///
/// Set by [`install_pidfile_cleanup_signals`] to a leaked, NUL-terminated C
/// string that lives for the rest of the process, so the handler can read it
/// without touching freed memory. The handler only loads this pointer.
static CLEANUP_PIDFILE: std::sync::atomic::AtomicPtr<libc::c_char> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Async-signal-safe handler that removes the pidfile, then re-raises the
/// signal so the default action (process termination) still runs.
pub(crate) extern "C" fn pidfile_cleanup_handler(signum: i32) {
    use std::sync::atomic::Ordering;

    let ptr = CLEANUP_PIDFILE.load(Ordering::Acquire);
    if !ptr.is_null() {
        // SAFETY: `ptr` is null or a valid NUL-terminated C string leaked in
        // `install_pidfile_cleanup_signals` (never freed), so it stays valid
        // for the life of the process. `unlink` is async-signal-safe.
        unsafe { libc::unlink(ptr) };
    }
    // The disposition was reset to default by SA_RESETHAND before this handler
    // ran; re-raise so the process terminates with a status reflecting the
    // signal.
    // SAFETY: `raise` is async-signal-safe and takes any signal number.
    unsafe { libc::raise(signum) };
}

/// Whether the pidfile cleanup handler may be installed on `sig` (R151).
///
/// [`pidfile_cleanup_handler`] re-raises into the default action, so `sig`'s
/// default must end the process: otherwise the daemon keeps running without
/// its pidfile. The accepted signals are the catchable ones POSIX defines as
/// terminating (some with a core dump) on every system, plus the real-time
/// range where the capability table knows it. SIGPIPE terminates too, but it
/// reports a write to a closed pipe, not a request to stop, and a Rust
/// process usually ignores it (R127), so it is left off. Anything else,
/// platform-specific signals included (SIGIO, SIGINFO, SIGPWR differ by OS),
/// is refused.
pub(crate) fn may_clean_up_on(sig: i32) -> bool {
    #[cfg(blivet_rt_signals_reserved)]
    if (libc::SIGRTMIN()..=libc::SIGRTMAX()).contains(&sig) {
        return true;
    }
    [
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGALRM,
        libc::SIGVTALRM,
        libc::SIGPROF,
        libc::SIGXCPU,
        libc::SIGXFSZ,
        libc::SIGABRT,
        libc::SIGBUS,
        libc::SIGFPE,
        libc::SIGILL,
        libc::SIGSEGV,
        libc::SIGSYS,
        libc::SIGTRAP,
    ]
    .contains(&sig)
}

/// Set `SIGPIPE`'s default disposition, before an `exec`, and return the one
/// it replaced.
///
/// Daemonizing keeps the caller's `SIGPIPE` disposition (R127), usually the
/// Rust runtime's `SIG_IGN`, and an ignored disposition survives `exec(2)`;
/// the exec'd program expects the default (R128). If the exec fails,
/// [`CallerSigpipe::restore`] puts the caller's disposition back.
pub(crate) fn restore_default_sigpipe() -> CallerSigpipe {
    let sa = new_sigaction(libc::SIG_DFL, 0);
    let mut old = zeroed_sigaction();
    // SAFETY: SIG_DFL with an empty mask is a valid disposition for SIGPIPE,
    // and `old` receives the one it replaces.
    unsafe { libc::sigaction(libc::SIGPIPE, &sa, &mut old) };
    CallerSigpipe(old)
}

/// The `SIGPIPE` disposition the caller had before an `exec` set the default.
pub(crate) struct CallerSigpipe(libc::sigaction);

impl CallerSigpipe {
    /// Put the caller's disposition back, after an `exec` that failed.
    pub(crate) fn restore(self) {
        // SAFETY: `self.0` is the disposition sigaction reported moments ago.
        unsafe { libc::sigaction(libc::SIGPIPE, &self.0, std::ptr::null_mut()) };
    }
}

/// Install [`pidfile_cleanup_handler`] for each signal in `signals`.
///
/// Stores `pidfile` (leaked, so it outlives any later free) for the handler to
/// unlink. Uses `SA_RESETHAND` so the handler runs once and the re-raise hits
/// the default action.
///
/// Refuses, before changing anything (R129), a signal outside the cleanup
/// allowlist (R151), and one whose disposition is neither the default nor
/// this handler (R149). Every signal on the allowlist can be caught, so the
/// install that follows does not fail on a supported OS; if it ever did, the
/// error names the signal and the earlier signals keep the handler.
pub(crate) fn install_pidfile_cleanup_signals(
    pidfile: &std::ffi::CStr,
    signals: &[i32],
) -> std::io::Result<()> {
    use std::sync::atomic::Ordering;
    #[cfg(test)]
    crate::test_support::assert_isolated("install_pidfile_cleanup_signals");

    // Refuse before changing anything if a signal already has a disposition
    // someone chose: another library's handler (signal-hook's, say) would be
    // silently replaced, and an ignored signal would start killing the
    // process. Step 9 reset every disposition but SIGPIPE's to the default,
    // so anything else was installed since. SIGPIPE, which keeps the Rust
    // runtime's SIG_IGN, is not on the allowlist. This call's own handler, from an earlier call,
    // may be replaced. A query that fails is left to the install loop, which
    // names the signal.
    let ours = pidfile_cleanup_handler as extern "C" fn(i32) as usize;
    for &sig in signals {
        if !may_clean_up_on(sig) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "signal {sig} is not one cleanup_on_signals accepts: it takes only catchable signals whose default action ends the process, SIGPIPE aside"
                ),
            ));
        }
        let theirs = disposition(sig).map(|now| now.sa_sigaction);
        if theirs.is_ok_and(|h| h != libc::SIG_DFL && h != ours) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("signal {sig} already has a handler or is ignored; not replacing it"),
            ));
        }
    }

    // Leak a stable copy of the path. Repeated installs leak the prior copy,
    // a small bounded cost for a rarely-repeated setup call.
    //
    // The path is published *before* the install loop so that in the common
    // single-call case a term signal arriving mid-setup still unlinks the
    // intended pidfile (the whole point of the feature). The unavoidable cost:
    // between this store and full install, a still-live prior handler would
    // unlink the new path. That install-loop window is inherent to non-atomic
    // multi-`sigaction` setup and cannot be closed without weakening the common
    // path (publishing last would leave the pidfile if a term signal lands in
    // the post-install/pre-publish gap).
    let leaked: *mut libc::c_char = pidfile.to_owned().into_raw();
    CLEANUP_PIDFILE.store(leaked, Ordering::Release);

    for &sig in signals {
        let sa = new_sigaction(ours, libc::SA_RESETHAND);
        // SAFETY: `sa` installs pidfile_cleanup_handler, which calls only
        // async-signal-safe functions.
        let ret = unsafe { libc::sigaction(sig, &sa, std::ptr::null_mut()) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            return Err(std::io::Error::new(
                err.kind(),
                format!("signal {sig}: {err}"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Covers: R164
    #[test]
    fn a_reset_failing_part_way_puts_back_what_it_replaced() {
        crate::test_support::run_in_subprocess(
            "signals::tests::a_reset_failing_part_way_puts_back_what_it_replaced_subprocess",
        );
    }

    /// SIGHUP and SIGINT sit below SIGTERM on every platform, and the
    /// failpoint fails at SIGTERM, so the step has replaced both first.
    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn a_reset_failing_part_way_puts_back_what_it_replaced_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
        let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        for sig in [Signal::SIGHUP, Signal::SIGINT] {
            // SAFETY: SIG_IGN installs no handler, so no code runs.
            unsafe { sigaction(sig, &ignore) }.unwrap();
        }
        let before = crate::test_support::signal_state();
        crate::steps::failpoints::SIGACTION_FAILS.set();

        let result = reset_signal_dispositions(Rollback::PutBack);

        let failed_at_sigterm = matches!(
            &result,
            Err(crate::error::DaemonizeError::SystemError(m)) if m.starts_with("sigaction(15)")
        );
        assert!(failed_at_sigterm, "{:?}", result.err());
        assert_eq!(crate::test_support::signal_state(), before);
    }

    // Covers: R99
    #[test]
    fn reset_signal_dispositions_restores_default() {
        crate::test_support::run_in_subprocess(
            "signals::tests::reset_signal_dispositions_restores_default_subprocess",
        );
    }

    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn reset_signal_dispositions_restores_default_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

        // Install SIG_IGN handler for SIGUSR1
        let handler = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        // SAFETY: SIG_IGN and SIG_DFL install no handler, so no code runs.
        let old = unsafe { sigaction(Signal::SIGUSR1, &handler) }.unwrap();

        // Verify it's not SIG_DFL
        // SAFETY: SIG_IGN and SIG_DFL install no handler, so no code runs.
        let current = unsafe { sigaction(Signal::SIGUSR1, &handler) }.unwrap();
        assert!(
            !matches!(current.handler(), SigHandler::SigDfl),
            "precondition: SIGUSR1 should not be SIG_DFL"
        );

        // Reset all dispositions
        reset_signal_dispositions(Rollback::Skip).unwrap().keep();

        // Read back SIGUSR1 disposition — should be SIG_DFL now
        // SAFETY: SIG_IGN and SIG_DFL install no handler, so no code runs.
        let after_reset = unsafe { sigaction(Signal::SIGUSR1, &old) }.unwrap();
        assert!(
            matches!(after_reset.handler(), SigHandler::SigDfl),
            "SIGUSR1 should be SIG_DFL after reset"
        );
    }

    // Covers: R127
    #[test]
    fn reset_signal_dispositions_preserves_sigpipe() {
        crate::test_support::run_in_subprocess(
            "signals::tests::reset_signal_dispositions_preserves_sigpipe_subprocess",
        );
    }

    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn reset_signal_dispositions_preserves_sigpipe_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

        // SIG_IGN (what the Rust runtime installs) must survive the reset:
        // resetting it would turn every write to a closed pipe/socket into
        // silent process death instead of an EPIPE error.
        let ign = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        // SAFETY: SIG_IGN and SIG_DFL install no handler, so no code runs.
        unsafe { sigaction(Signal::SIGPIPE, &ign) }.unwrap();
        reset_signal_dispositions(Rollback::Skip).unwrap().keep();
        // SAFETY: SIG_IGN and SIG_DFL install no handler, so no code runs.
        let after = unsafe { sigaction(Signal::SIGPIPE, &ign) }.unwrap();
        assert!(
            matches!(after.handler(), SigHandler::SigIgn),
            "SIGPIPE SIG_IGN must survive the disposition reset"
        );

        // The reset preserves the caller's choice, whatever it is — it does
        // not force SIG_IGN: an explicit SIG_DFL stays SIG_DFL.
        let dfl = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        // SAFETY: SIG_IGN and SIG_DFL install no handler, so no code runs.
        unsafe { sigaction(Signal::SIGPIPE, &dfl) }.unwrap();
        reset_signal_dispositions(Rollback::Skip).unwrap().keep();
        // SAFETY: SIG_IGN and SIG_DFL install no handler, so no code runs.
        let after = unsafe { sigaction(Signal::SIGPIPE, &dfl) }.unwrap();
        assert!(
            matches!(after.handler(), SigHandler::SigDfl),
            "an explicit SIG_DFL must also be preserved, not overridden"
        );
    }

    use crate::test_support::{is_subprocess, run_in_subprocess};
    use std::sync::atomic::Ordering;

    // Covers: R151
    #[test]
    fn only_signals_posix_has_terminate_qualify_for_cleanup() {
        for sig in [
            libc::SIGHUP,
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGTERM,
            libc::SIGUSR1,
            libc::SIGUSR2,
            libc::SIGALRM,
            libc::SIGVTALRM,
            libc::SIGPROF,
            libc::SIGXCPU,
            libc::SIGXFSZ,
            libc::SIGABRT,
            libc::SIGBUS,
            libc::SIGFPE,
            libc::SIGILL,
            libc::SIGSEGV,
            libc::SIGSYS,
            libc::SIGTRAP,
        ] {
            assert!(
                may_clean_up_on(sig),
                "signal {sig} terminates by default everywhere"
            );
        }
        // Ignored, stopping, or continuing everywhere, and SIGIO, which the
        // BSDs and macOS ignore by default while Linux terminates. SIGKILL
        // and SIGSTOP cannot be caught. SIGPIPE terminates by default, but
        // the daemon keeps it ignored (R127), and a handler would turn a
        // write to a closed pipe from EPIPE into the end of the daemon.
        for sig in [
            libc::SIGKILL,
            libc::SIGSTOP,
            libc::SIGPIPE,
            libc::SIGCHLD,
            libc::SIGURG,
            libc::SIGWINCH,
            libc::SIGCONT,
            libc::SIGTSTP,
            libc::SIGTTIN,
            libc::SIGTTOU,
            libc::SIGIO,
            0,
            1000,
        ] {
            assert!(!may_clean_up_on(sig), "signal {sig} is not on the list");
        }
    }

    // Covers: R151
    #[cfg(blivet_rt_signals_reserved)]
    #[test]
    fn real_time_signals_qualify_for_cleanup() {
        assert!(may_clean_up_on(libc::SIGRTMIN()));
        assert!(may_clean_up_on(libc::SIGRTMAX()));
    }

    /// Current disposition of `sig` (the `sa_sigaction` word), read without
    /// changing it.
    fn current_disposition(sig: i32) -> usize {
        disposition(sig)
            .unwrap_or_else(|e| panic!("sigaction query for signal {sig} failed: {e}"))
            .sa_sigaction
    }

    // Covers: R129
    #[test]
    fn a_refused_slice_changes_nothing() {
        run_in_subprocess("signals::tests::a_refused_slice_changes_nothing_subprocess");
    }

    /// Every refusal is decided before the first handler goes in: a slice
    /// whose last signal is refused leaves the signals before it, and the
    /// handler's pidfile path, as they were. Mutates process-global signal
    /// state, hence the subprocess isolation.
    #[test]
    #[ignore]
    fn a_refused_slice_changes_nothing_subprocess() {
        if !is_subprocess() {
            return;
        }

        // A prior install, so an unchanged path is distinguishable from null.
        install_pidfile_cleanup_signals(c"/a/prior.pid", &[libc::SIGUSR2])
            .expect("SIGUSR2 install should succeed");
        let prior_ptr = CLEANUP_PIDFILE.load(Ordering::Acquire);
        let prior_usr1 = current_disposition(libc::SIGUSR1);

        install_pidfile_cleanup_signals(c"/a/new.pid", &[libc::SIGUSR1, libc::SIGCHLD])
            .expect_err("SIGCHLD does not end the process, so it is refused");

        assert_eq!(
            (
                current_disposition(libc::SIGUSR1),
                CLEANUP_PIDFILE.load(Ordering::Acquire)
            ),
            (prior_usr1, prior_ptr),
            "a refused slice changed SIGUSR1's disposition or the pidfile path"
        );
    }
}
