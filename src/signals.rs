//! Signal handling: the step-9 reset of every disposition, the pidfile
//! cleanup handler behind `DaemonContext::cleanup_on_signals`, and the
//! `SIGPIPE` restore before an exec.
//!
//! The `sigaction` and `signal` FFI this needs is allowed at module level,
//! next to the code whose invariants it upholds.

#![allow(unsafe_code)]

/// Reset signal dispositions from 1 through the signal ceiling to SIG_DFL.
///
/// On Linux, iterates standard signals (1..32) then real-time signals
/// (SIGRTMIN..=SIGRTMAX), skipping the NPTL-reserved range (32..SIGRTMIN).
/// On other platforms, iterates 1..=64 and silently skips EINVAL.
///
/// SIGKILL and SIGSTOP are always skipped (cannot be caught/reset).
///
/// SIGPIPE is also skipped, preserving the caller's disposition (R127): the
/// Rust runtime installs SIG_IGN so writes to a closed pipe/socket return
/// `EPIPE` instead of killing the process, and resetting it would silently
/// revoke that for the entire daemon — including the library's own
/// `notify_parent` write, whose documented `NotifyFailed` error could then
/// never be observed. The CLI restores SIG_DFL just before `exec` (R128), so
/// exec'd programs still start with the conventional disposition.
///
/// Returns [`SystemError`](crate::error::DaemonizeError::SystemError) on a
/// non-`EINVAL` `sigaction` failure (POSIX documents only `EINVAL` for these
/// arguments, so this is a should-not-happen guard) so the caller reports it
/// to the parent rather than panicking. `EINVAL` — an uncatchable or invalid
/// signal number — is skipped silently.
pub(crate) fn reset_signal_dispositions() -> Result<(), crate::error::DaemonizeError> {
    #[cfg(test)]
    crate::test_support::assert_isolated("reset_signal_dispositions");
    #[cfg(test)]
    if crate::steps::failpoints::SIGACTION_FAILS.injected() {
        return Err(crate::error::DaemonizeError::SystemError(
            "sigaction(1): injected failure".into(),
        ));
    }
    let signals = signal_range();
    for sig in signals {
        if matches!(sig, libc::SIGKILL | libc::SIGSTOP | libc::SIGPIPE) {
            continue;
        }

        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = libc::SIG_DFL;
        unsafe { libc::sigemptyset(&mut sa.sa_mask) };
        sa.sa_flags = 0;

        let ret = unsafe { libc::sigaction(sig, &sa, std::ptr::null_mut()) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EINVAL) {
                return Err(crate::error::DaemonizeError::SystemError(format!(
                    "sigaction({sig}): {err}"
                )));
            }
            // EINVAL: signal cannot be caught, skip
        }
    }
    Ok(())
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
    // signal. `raise` is async-signal-safe.
    unsafe { libc::raise(signum) };
}

/// Whether `sig`'s default action ends the process (R151).
///
/// [`pidfile_cleanup_handler`] re-raises into the default action. On a signal
/// whose default is to ignore, stop, or continue, it would remove the pidfile
/// and leave the daemon running. The list holds the signals every supported
/// platform treats that way.
pub(crate) fn terminates_by_default(sig: i32) -> bool {
    ![
        libc::SIGCHLD,
        libc::SIGURG,
        libc::SIGWINCH,
        libc::SIGCONT,
        libc::SIGTSTP,
        libc::SIGTTIN,
        libc::SIGTTOU,
    ]
    .contains(&sig)
}

/// Restore `SIGPIPE`'s default disposition, before an `exec`.
///
/// Daemonizing keeps the Rust runtime's ignored `SIGPIPE` (R127), and an
/// ignored disposition survives `exec(2)`; the exec'd program expects the
/// default (R128).
pub(crate) fn restore_default_sigpipe() {
    // SAFETY: SIG_DFL is a valid disposition for SIGPIPE.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
}

/// Install [`pidfile_cleanup_handler`] for each signal in `signals`.
///
/// Stores `pidfile` (leaked, so it outlives any later free) for the handler to
/// unlink. Uses `SA_RESETHAND` so the handler runs once and the re-raise hits
/// the default action.
///
/// All-or-nothing (R129): if `sigaction` fails for any signal (e.g. EINVAL
/// for one that cannot be caught, like SIGKILL/SIGSTOP), the dispositions
/// already replaced for earlier signals in the slice and the prior pidfile
/// pointer are restored, and the returned error names the failing signal.
///
/// Refuses (R149), before changing anything, a signal whose disposition is
/// neither the default nor this handler, and (R151) one whose default action
/// does not terminate the process.
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
    // so anything else was installed since. SIGPIPE keeps the Rust runtime's
    // SIG_IGN and is refused like any ignored signal. This call's own handler, from an earlier call,
    // may be replaced. A query that fails is left to the install loop, which
    // names the signal.
    let ours = pidfile_cleanup_handler as extern "C" fn(i32) as usize;
    for &sig in signals {
        if !terminates_by_default(sig) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("signal {sig} does not terminate the process by default; its cleanup would remove a running daemon's pidfile"),
            ));
        }
        let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a null new action only reads the disposition into `current`.
        let queried = unsafe { libc::sigaction(sig, std::ptr::null(), &mut current) } == 0;
        if queried && current.sa_sigaction != libc::SIG_DFL && current.sa_sigaction != ours {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("signal {sig} already has a handler or is ignored; not replacing it"),
            ));
        }
    }

    // Leak a stable copy of the path. Repeated installs leak the prior copy,
    // a small bounded cost for a rarely-repeated setup call. The prior pointer
    // (null or an earlier leaked path, both valid forever) is kept for rollback.
    //
    // The path is published *before* the install loop so that in the common
    // single-call case a term signal arriving mid-setup still unlinks the
    // intended pidfile (the whole point of the feature). The unavoidable cost:
    // between this swap and full install, a still-live prior handler would
    // unlink the new path. That install-loop window is inherent to non-atomic
    // multi-`sigaction` setup and cannot be closed without weakening the common
    // path (publishing last would leave the pidfile if a term signal lands in
    // the post-install/pre-publish gap).
    let leaked: *mut libc::c_char = pidfile.to_owned().into_raw();
    let prior_path = CLEANUP_PIDFILE.swap(leaked, Ordering::AcqRel);

    // Dispositions replaced so far, so a failure can restore them.
    let mut replaced: Vec<(i32, libc::sigaction)> = Vec::with_capacity(signals.len());

    for &sig in signals {
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        // Coerce to a fn pointer before the usize cast (a bare fn-item cast
        // trips clippy::fn_to_numeric_cast); sa_sigaction is pointer-sized.
        let handler = pidfile_cleanup_handler as extern "C" fn(i32);
        sa.sa_sigaction = handler as usize;
        unsafe { libc::sigemptyset(&mut sa.sa_mask) };
        sa.sa_flags = libc::SA_RESETHAND;

        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::sigaction(sig, &sa, &mut old) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            // Restore the pointer first, then the dispositions: any handler
            // still live during the restore loop (a prior install's, or one this
            // call installed and hasn't reverted yet) then reads the prior path,
            // matching the pre-call state instead of unlinking the new path.
            CLEANUP_PIDFILE.store(prior_path, Ordering::Release);
            for (s, prev) in replaced.iter().rev() {
                // SAFETY: `prev` is the exact disposition sigaction reported
                // for `s` moments ago; re-installing it is always valid.
                unsafe { libc::sigaction(*s, prev, std::ptr::null_mut()) };
            }
            return Err(std::io::Error::new(
                err.kind(),
                format!("signal {sig}: {err}"),
            ));
        }
        replaced.push((sig, old));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

        // Install SIG_IGN handler for SIGUSR1
        let handler = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        let old = unsafe { sigaction(Signal::SIGUSR1, &handler) }.unwrap();

        // Verify it's not SIG_DFL
        let current = unsafe { sigaction(Signal::SIGUSR1, &handler) }.unwrap();
        assert!(
            !matches!(current.handler(), SigHandler::SigDfl),
            "precondition: SIGUSR1 should not be SIG_DFL"
        );

        // Reset all dispositions
        reset_signal_dispositions().unwrap();

        // Read back SIGUSR1 disposition — should be SIG_DFL now
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
        use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

        // SIG_IGN (what the Rust runtime installs) must survive the reset:
        // resetting it would turn every write to a closed pipe/socket into
        // silent process death instead of an EPIPE error.
        let ign = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        unsafe { sigaction(Signal::SIGPIPE, &ign) }.unwrap();
        reset_signal_dispositions().unwrap();
        let after = unsafe { sigaction(Signal::SIGPIPE, &ign) }.unwrap();
        assert!(
            matches!(after.handler(), SigHandler::SigIgn),
            "SIGPIPE SIG_IGN must survive the disposition reset"
        );

        // The reset preserves the caller's choice, whatever it is — it does
        // not force SIG_IGN: an explicit SIG_DFL stays SIG_DFL.
        let dfl = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        unsafe { sigaction(Signal::SIGPIPE, &dfl) }.unwrap();
        reset_signal_dispositions().unwrap();
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
    fn only_signals_that_terminate_by_default_qualify_for_cleanup() {
        for sig in [
            libc::SIGUSR1,
            libc::SIGUSR2,
            libc::SIGHUP,
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGTERM,
            libc::SIGALRM,
        ] {
            assert!(
                terminates_by_default(sig),
                "signal {sig} terminates by default"
            );
        }
        for sig in [
            libc::SIGCHLD,
            libc::SIGURG,
            libc::SIGWINCH,
            libc::SIGCONT,
            libc::SIGTSTP,
            libc::SIGTTIN,
            libc::SIGTTOU,
        ] {
            assert!(
                !terminates_by_default(sig),
                "signal {sig} does not terminate by default"
            );
        }
    }

    /// Current disposition of `sig` (the `sa_sigaction` word), read without
    /// changing it.
    fn current_disposition(sig: i32) -> usize {
        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::sigaction(sig, std::ptr::null(), &mut old) };
        assert_eq!(ret, 0, "sigaction query for signal {sig} failed");
        old.sa_sigaction
    }

    // Covers: R129
    #[test]
    fn failed_install_rolls_back() {
        run_in_subprocess("signals::tests::failed_install_rolls_back_subprocess");
    }

    /// A failing install must leave no trace: handlers installed for signals
    /// earlier in the slice are restored, the failing signal is named in the
    /// error, and the handler's pidfile pointer reverts to its prior value.
    /// Mutates process-global signal state, hence the subprocess isolation.
    #[test]
    #[ignore]
    fn failed_install_rolls_back_subprocess() {
        if !is_subprocess() {
            return;
        }

        // Establish a prior successful install so pointer rollback is
        // distinguishable from "still null".
        install_pidfile_cleanup_signals(c"/a/rollback-prior.pid", &[libc::SIGUSR2])
            .expect("SIGUSR2 install should succeed");
        let prior_ptr = CLEANUP_PIDFILE.load(Ordering::Acquire);
        let prior_usr1 = current_disposition(libc::SIGUSR1);

        // SIGUSR1 installs, then SIGKILL fails with EINVAL.
        let err = install_pidfile_cleanup_signals(
            c"/a/rollback-new.pid",
            &[libc::SIGUSR1, libc::SIGKILL],
        )
        .expect_err("SIGKILL cannot be caught");

        assert!(
            err.to_string()
                .contains(&format!("signal {}", libc::SIGKILL)),
            "error should name the failing signal, got: {err}"
        );
        assert_eq!(
            current_disposition(libc::SIGUSR1),
            prior_usr1,
            "SIGUSR1 disposition should be rolled back after the failed install"
        );
        assert_eq!(
            CLEANUP_PIDFILE.load(Ordering::Acquire),
            prior_ptr,
            "pidfile pointer should revert to the prior install's path"
        );
        // The prior install must keep working: SIGUSR2 still has the handler.
        assert_eq!(
            current_disposition(libc::SIGUSR2),
            pidfile_cleanup_handler as extern "C" fn(i32) as usize,
            "earlier successful install should be untouched"
        );
    }
}
