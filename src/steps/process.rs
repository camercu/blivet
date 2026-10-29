//! Steps that set process-wide state: umask (step 4), working directory
//! (step 5), signal mask (step 10), and environment (step 11). Each returns
//! the [`Undo`] that puts its change back.

use std::ffi::OsString;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::Path;

use nix::fcntl::OFlag;
use nix::sys::signal::SigSet;
use nix::sys::stat::Mode;

use super::{Rollback, Saved, Undo};
use crate::error::DaemonizeError;

/// Step 4: Set process umask.
///
/// `mode` is an octal permission value (validated `<= 0o7777` by
/// [`DaemonConfig::validate`](crate::DaemonConfig::validate)); the cast to
/// `mode_t` is therefore lossless.
#[allow(clippy::cast_possible_truncation)] // validated <= 0o7777; mode_t is u16 on macOS and FreeBSD
pub(crate) fn set_umask(mode: u32, rollback: Rollback) -> Undo {
    #[cfg(test)]
    crate::test_support::assert_isolated("set_umask");
    let was = nix::sys::stat::umask(Mode::from_bits_truncate(mode as libc::mode_t));
    Undo::saving(rollback, || Saved::Umask(Umask(was)))
}

/// The umask step 4 replaced.
pub(crate) struct Umask(Mode);

impl Umask {
    pub(crate) fn restore(self) {
        nix::sys::stat::umask(self.0);
    }
}

/// Step 5: Change working directory.
pub(crate) fn change_dir(path: &Path, rollback: Rollback) -> Result<Undo, DaemonizeError> {
    #[cfg(test)]
    crate::test_support::assert_isolated("change_dir");
    let undo = Undo::saving(rollback, || Saved::Cwd(Cwd::save()));
    nix::unistd::chdir(path)
        .map_err(|e| DaemonizeError::ChdirFailed(format!("{}: {e}", path.display())))?;
    Ok(undo)
}

/// A descriptor of the working directory step 5 left, to `fchdir` back to.
///
/// Best-effort: a directory the caller may enter but not read cannot be
/// opened, and an undo aid must not fail the start (R164).
pub(crate) struct Cwd(Option<OwnedFd>);

impl Cwd {
    fn save() -> Self {
        Self(super::open_above_stdio(".", OFlag::O_RDONLY, Mode::empty()).ok())
    }

    pub(crate) fn restore(self) {
        if let Some(cwd) = self.0 {
            let _ = nix::unistd::fchdir(cwd.as_fd());
        }
    }

    pub(crate) fn held_fd(&self) -> Option<i32> {
        self.0.as_ref().map(AsRawFd::as_raw_fd)
    }
}

/// Step 10: Clear the signal mask.
///
/// Returns [`SystemError`](DaemonizeError::SystemError) if `sigprocmask` fails
/// (e.g. blocked by a seccomp filter) so the caller can report it.
pub(crate) fn clear_signal_mask(rollback: Rollback) -> Result<Undo, DaemonizeError> {
    use nix::sys::signal::SigmaskHow;
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
    Ok(Undo::saving(rollback, || {
        Saved::SignalMask(SignalMask(was))
    }))
}

/// The signal mask step 10 replaced.
pub(crate) struct SignalMask(SigSet);

impl SignalMask {
    pub(crate) fn restore(self) {
        let _ = self.0.thread_set_mask();
    }
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
/// The token restores only before `init`: it calls `setenv` too.
#[allow(unsafe_code)]
#[allow(clippy::disallowed_methods)] // setenv thread safety: single thread, per the contract
pub(crate) fn set_env_vars(env: &[(String, String)], rollback: Rollback) -> Undo {
    #[cfg(test)]
    crate::test_support::assert_isolated("set_env_vars");
    let mut was = Vec::new();
    for (key, value) in env {
        if let Rollback::PutBack = rollback {
            was.push((key.clone(), std::env::var_os(key)));
        }
        // SAFETY: single-threaded per this fn's contract (post-fork child or
        // foreground entry gate), so the `setenv` cannot race.
        unsafe { std::env::set_var(key, value) };
    }
    Undo::saving(rollback, || Saved::EnvVars(EnvVars(was)))
}

/// Each variable step 11 set, with the value it had before, in the order set.
pub(crate) struct EnvVars(Vec<(String, Option<OsString>)>);

impl EnvVars {
    /// Latest first, so a key set twice ends at the value it had before
    /// either. Single-threaded, as `set_env_vars`.
    #[allow(unsafe_code)]
    #[allow(clippy::disallowed_methods)] // setenv thread safety: before init, single thread
    pub(crate) fn restore(self) {
        for (key, value) in self.0.into_iter().rev() {
            match value {
                // SAFETY: the sequence restores this token only before init,
                // on a single thread, so the `setenv` cannot race another
                // thread's `environ` read.
                Some(value) => unsafe { std::env::set_var(&key, value) },
                // SAFETY: as above, for `unsetenv`.
                None => unsafe { std::env::remove_var(&key) },
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn set_umask_applies_and_can_be_read_back() {
        if !crate::test_support::isolated() {
            return;
        }
        // Start from a different umask, so a set_umask that did nothing fails.
        nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
        set_umask(0o022, Rollback::Skip).keep();
        let readback = nix::sys::stat::umask(Mode::from_bits_truncate(0o077));
        assert_eq!(readback, Mode::from_bits_truncate(0o022));
    }

    #[test]
    fn change_dir_to_tempdir() {
        if !crate::test_support::isolated() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let result = change_dir(tmp.path(), Rollback::Skip).map(Undo::keep);
        assert!(result.is_ok());
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(cwd, std::fs::canonicalize(tmp.path()).unwrap());
    }

    #[test]
    fn change_dir_nonexistent_fails() {
        if !crate::test_support::isolated() {
            return;
        }
        let result = change_dir(
            Path::new("/nonexistent_daemonize_test_path"),
            Rollback::Skip,
        )
        .map(Undo::keep);
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

        clear_signal_mask(Rollback::Skip).unwrap().keep();

        let mut current = SigSet::empty();
        nix::sys::signal::sigprocmask(SigmaskHow::SIG_SETMASK, None, Some(&mut current)).unwrap();
        assert!(!current.contains(Signal::SIGUSR1));
    }

    #[test]
    #[allow(unsafe_code)]
    fn set_env_vars_applies() {
        if !crate::test_support::isolated() {
            return;
        }
        let vars = vec![
            ("DAEMONIZE_TEST_A".into(), "1".into()),
            ("DAEMONIZE_TEST_B".into(), "2".into()),
        ];
        set_env_vars(&vars, Rollback::Skip).keep();
        assert_eq!(std::env::var("DAEMONIZE_TEST_A").unwrap(), "1");
        assert_eq!(std::env::var("DAEMONIZE_TEST_B").unwrap(), "2");
    }

    #[test]
    #[allow(unsafe_code)]
    fn set_env_vars_last_write_wins() {
        if !crate::test_support::isolated() {
            return;
        }
        let vars = vec![
            ("DAEMONIZE_TEST_DUP".into(), "first".into()),
            ("DAEMONIZE_TEST_DUP".into(), "second".into()),
        ];
        set_env_vars(&vars, Rollback::Skip).keep();
        assert_eq!(std::env::var("DAEMONIZE_TEST_DUP").unwrap(), "second");
    }
}
