//! Steps that set process-wide state: umask (step 4), working directory
//! (step 5), signal mask (step 10), and environment (step 11).

use std::ffi::OsString;
use std::path::Path;

use nix::sys::stat::Mode;

use crate::error::DaemonizeError;

/// Step 4: Set process umask.
///
/// `mode` is an octal permission value (validated `<= 0o7777` by
/// [`DaemonConfig::validate`](crate::DaemonConfig::validate)); the cast to
/// `mode_t` is therefore lossless.
/// Returns the umask it replaced, for a foreground failure to put back.
pub(crate) fn set_umask(mode: u32) -> Mode {
    #[cfg(test)]
    crate::test_support::assert_isolated("set_umask");
    nix::sys::stat::umask(Mode::from_bits_truncate(mode as libc::mode_t))
}

/// Step 5: Change working directory.
pub(crate) fn change_dir(path: &Path) -> Result<(), DaemonizeError> {
    #[cfg(test)]
    crate::test_support::assert_isolated("change_dir");
    nix::unistd::chdir(path)
        .map_err(|e| DaemonizeError::ChdirFailed(format!("{}: {e}", path.display())))
}

/// Step 10: Clear the signal mask.
///
/// Returns [`SystemError`](DaemonizeError::SystemError) if `sigprocmask` fails
/// (e.g. blocked by a seccomp filter) so the caller can report it. Returns
/// the mask it replaced, for a foreground failure to put back (R164).
pub(crate) fn clear_signal_mask() -> Result<nix::sys::signal::SigSet, DaemonizeError> {
    use nix::sys::signal::{SigSet, SigmaskHow};
    #[cfg(test)]
    if crate::steps::failpoints::SIGPROCMASK_FAILS.injected() {
        return Err(DaemonizeError::SystemError(
            "sigprocmask: injected failure".into(),
        ));
    }
    let mut was = SigSet::empty();
    nix::sys::signal::sigprocmask(
        SigmaskHow::SIG_SETMASK,
        Some(&SigSet::empty()),
        Some(&mut was),
    )
    .map_err(|e| DaemonizeError::SystemError(format!("sigprocmask: {e}")))?;
    Ok(was)
}

/// Step 11: Set environment variables in insertion order.
///
/// Uses `std::env::set_var`, which is not thread-safe. Sound here because this
/// runs only inside the daemonization sequence: post-fork the child is
/// single-threaded by `fork` semantics, and in foreground mode the entry point
/// ([`daemonize`](crate::daemonize) /
/// [`daemonize_unchecked`](crate::daemonize_unchecked)) requires
/// single-threadedness. No other thread can touch `environ`.
///
/// Infallible for a validated config: `set_var` panics only on an empty key,
/// `=` or NUL in the key, or NUL in the value — all rejected by
/// [`DaemonConfig::validate`](crate::DaemonConfig::validate) (R36, R138).
///
/// Returns each variable's previous value, in the order set, for
/// [`restore_env_vars`] to put back after a foreground failure (R164).
#[allow(unsafe_code)]
pub(crate) fn set_env_vars(env: &[(String, String)]) -> Vec<(String, Option<OsString>)> {
    #[cfg(test)]
    crate::test_support::assert_isolated("set_env_vars");
    let mut was = Vec::with_capacity(env.len());
    for (key, value) in env {
        was.push((key.clone(), std::env::var_os(key)));
        // SAFETY: single-threaded per this fn's contract (post-fork child or
        // foreground entry gate), so the `setenv` cannot race.
        unsafe { std::env::set_var(key, value) };
    }
    was
}

/// Puts back what [`set_env_vars`] replaced, latest first, so a key set twice
/// ends at the value it had before either. Single-threaded, as `set_env_vars`.
#[allow(unsafe_code)]
pub(crate) fn restore_env_vars(was: &mut Vec<(String, Option<OsString>)>) {
    while let Some((key, value)) = was.pop() {
        // SAFETY: as set_env_vars.
        match value {
            Some(value) => unsafe { std::env::set_var(&key, value) },
            None => unsafe { std::env::remove_var(&key) },
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn set_umask_applies_and_can_be_read_back() {
        crate::test_support::run_in_subprocess(
            "steps::process::tests::set_umask_applies_and_can_be_read_back_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn set_umask_applies_and_can_be_read_back_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        // Start from a different umask, so a set_umask that did nothing fails.
        nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
        set_umask(0o022);
        let readback = nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
        assert_eq!(readback, Mode::from_bits_truncate(0o022));
    }

    #[test]
    fn change_dir_to_tempdir() {
        crate::test_support::run_in_subprocess(
            "steps::process::tests::change_dir_to_tempdir_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn change_dir_to_tempdir_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let result = change_dir(tmp.path());
        assert!(result.is_ok());
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(cwd, std::fs::canonicalize(tmp.path()).unwrap());
    }

    #[test]
    fn change_dir_nonexistent_fails() {
        crate::test_support::run_in_subprocess(
            "steps::process::tests::change_dir_nonexistent_fails_subprocess",
        );
    }

    #[test]
    #[ignore]
    fn change_dir_nonexistent_fails_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let result = change_dir(Path::new("/nonexistent_daemonize_test_path"));
        assert!(matches!(result, Err(DaemonizeError::ChdirFailed(_))));
    }

    // sigprocmask changes only the calling thread's mask, so this runs in the
    // shared test process.
    #[test]
    fn clear_signal_mask_empties() {
        use nix::sys::signal::{SigSet, SigmaskHow, Signal};
        // Block SIGUSR1
        let mut set = SigSet::empty();
        set.add(Signal::SIGUSR1);
        nix::sys::signal::sigprocmask(SigmaskHow::SIG_BLOCK, Some(&set), None).unwrap();

        clear_signal_mask().unwrap();

        let mut current = SigSet::empty();
        nix::sys::signal::sigprocmask(SigmaskHow::SIG_SETMASK, None, Some(&mut current)).unwrap();
        assert!(!current.contains(Signal::SIGUSR1));
    }

    #[test]
    fn set_env_vars_applies() {
        crate::test_support::run_in_subprocess(
            "steps::process::tests::set_env_vars_applies_subprocess",
        );
    }

    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn set_env_vars_applies_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let vars = vec![
            ("DAEMONIZE_TEST_A".into(), "1".into()),
            ("DAEMONIZE_TEST_B".into(), "2".into()),
        ];
        set_env_vars(&vars);
        assert_eq!(std::env::var("DAEMONIZE_TEST_A").unwrap(), "1");
        assert_eq!(std::env::var("DAEMONIZE_TEST_B").unwrap(), "2");
    }

    #[test]
    fn set_env_vars_last_write_wins() {
        crate::test_support::run_in_subprocess(
            "steps::process::tests::set_env_vars_last_write_wins_subprocess",
        );
    }

    #[test]
    #[ignore]
    #[allow(unsafe_code)]
    fn set_env_vars_last_write_wins_subprocess() {
        if !crate::test_support::is_subprocess() {
            return;
        }
        let vars = vec![
            ("DAEMONIZE_TEST_DUP".into(), "first".into()),
            ("DAEMONIZE_TEST_DUP".into(), "second".into()),
        ];
        set_env_vars(&vars);
        assert_eq!(std::env::var("DAEMONIZE_TEST_DUP").unwrap(), "second");
    }
}
