//! The daemonize sequence: the forks, the post-fork steps in order, the
//! guard that undoes them when the sequence aborts, and the
//! launcher's side of the notification channel.
//!
//! The entry points in `lib.rs` validate and check the thread count, then
//! call [`daemonize_inner_with`]; the individual steps live in `steps/`.

use std::os::fd::{AsRawFd, OwnedFd};

use nix::unistd::ForkResult;

use crate::forker::Forker;
use crate::identity::ResolvedIdentity;
use crate::notify::{self, NotifyChannel};
use crate::{DaemonConfig, DaemonContext, DaemonizeError, ErrorKind, signals, steps};

/// Internal daemonization logic, generic over the Forker trait for testability.
///
/// # Safety
///
/// With a real `Forker` this performs `fork`; the process must be
/// single-threaded at the call, since forking with other threads running is
/// undefined behavior. The checked [`daemonize`](crate::daemonize)
/// establishes this, and the public
/// [`daemonize_unchecked`](crate::daemonize_unchecked) forwards the contract
/// to its caller. (The test `NullForker` does not fork, so test calls are
/// sound.)
#[allow(unsafe_code)]
pub(crate) unsafe fn daemonize_inner(
    config: &DaemonConfig,
    forker: &mut impl Forker,
) -> Result<DaemonContext, DaemonizeError> {
    // SAFETY: forwarded; this function has the same contract.
    unsafe { daemonize_inner_with(config, forker, || Ok(())) }.map(|(ctx, ())| ctx)
}

/// [`daemonize_inner`], running `init` in the final process at step 15.
///
/// # Safety
///
/// As [`daemonize_inner`]. `init` runs inside the single-threaded window.
#[allow(unsafe_code)]
pub(crate) unsafe fn daemonize_inner_with<T>(
    config: &DaemonConfig,
    forker: &mut impl Forker,
    init: impl FnOnce() -> Result<T, DaemonizeError>,
) -> Result<(DaemonContext, T), DaemonizeError> {
    let foreground = config.foreground;

    // Steps 1–3: Fork sequence (skipped in foreground mode). `None` here
    // means foreground; the context turns it into its launcher state.
    let mut pipe_wr = if foreground {
        None
    } else {
        // Step 1: Create the notification channel and first fork
        // Made in the caller's process, before any fork, so a failure
        // returns to the caller.
        let (pipe_rd, pipe_wr) = forker.create_notification_pipe()?;
        let pipe_wr = NotifyChannel::new(pipe_wr);

        // SAFETY: daemonize_unchecked() is unsafe and requires the caller to
        // ensure the process is single-threaded. The checked daemonize()
        // verifies this via the kernel thread count before calling
        // daemonize_inner().
        let first_fork = unsafe { forker.fork() };
        let first_fork = match first_fork {
            Ok(result) => result,
            Err(e) => {
                // No child exists yet; the error returns to the caller
                // directly, so close the write end silently rather than let the
                // NotifyChannel Drop safety net write into a channel whose only
                // reader is this same process.
                pipe_wr.close();
                return Err(e.into());
            }
        };
        match first_fork {
            ForkResult::Parent { .. } => {
                // Parent: close the write end *silently* (it only reads) then
                // read and exit. A plain drop would trip the NotifyChannel Drop
                // safety net and make the parent read its own failure bytes.
                pipe_wr.close();
                parent_pipe_reader(pipe_rd, forker);
            }
            ForkResult::Child => {
                // Child: close read end, continue
                drop(pipe_rd);
            }
        }

        // Step 2: setsid
        if let Err(e) = forker.setsid() {
            let e = DaemonizeError::from(e);
            pipe_wr.signal_error(&e);
            forker.exit(e.exit_code() as i32);
        }

        // Step 3: Second fork
        // SAFETY: same as above — single-threaded post-fork child.
        match unsafe { forker.fork() } {
            Ok(ForkResult::Parent { .. }) => {
                // Intermediate child exits silently: the grandchild daemon is
                // the sole writer, so close the write-end copy without tripping
                // the NotifyChannel Drop safety net.
                pipe_wr.close();
                forker.exit(0);
            }
            Ok(ForkResult::Child) => {
                // Grandchild continues
            }
            Err(e) => {
                let e = DaemonizeError::from(e);
                pipe_wr.signal_error(&e);
                forker.exit(e.exit_code() as i32);
            }
        }

        Some(pipe_wr)
    };

    // Steps 4–18 run in the final daemon process and report failures as a
    // single Result. In foreground mode no fork happened, so errors simply
    // propagate to the caller. In daemon mode the child cannot return to the
    // original caller, so funnel any error through one place: notify the
    // parent and exit. `pipe_wr` stays available so the error path can still
    // signal it.
    // SAFETY: this function's own contract requires a single thread.
    let result = unsafe { run_post_fork(config, &mut pipe_wr, init) };
    match result {
        Ok(done) => Ok(done),
        Err(e) if foreground => Err(e),
        Err(e) => {
            if let Some(pipe) = pipe_wr.take() {
                pipe.signal_error(&e);
            }
            forker.exit(e.exit_code() as i32);
        }
    }
}

/// The error a daemon reports for a panic in the sequence: exit 70
/// (`EX_SOFTWARE`), with the panic's text when it has one.
fn panicked(payload: &(dyn std::any::Any + Send)) -> ErrorKind {
    let text = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message");
    ErrorKind::application(70, format!("daemon panicked during startup: {text}"))
}

/// Steps 4–18: apply the configuration in the final daemon process.
///
/// Forker-free and fallible: every step returns its error rather than touching
/// the notification channel, leaving the single error-to-parent seam in
/// [`daemonize_inner_with`]. On success the notification channel's write end is moved into
/// the returned [`DaemonContext`]; `pipe_wr` is left `None`.
///
/// # Safety
///
/// No other thread may be running: step 17 calls `setenv` when the identity
/// switch sets a user.
#[allow(unsafe_code)]
unsafe fn run_post_fork<T>(
    config: &DaemonConfig,
    pipe_wr: &mut Option<NotifyChannel>,
    init: impl FnOnce() -> Result<T, DaemonizeError>,
) -> Result<(DaemonContext, T), DaemonizeError> {
    // What a failure must undo (R164): everything restorable in foreground
    // mode, where the process is the caller's; in a daemon, whose process
    // exits on failure, only what it left on disk.
    let mut abort = steps::AbortGuard::new(config.foreground);
    // SAFETY: this function's contract, passed on.
    let run = || unsafe { run_steps(config, pipe_wr, init, &mut abort) };
    let result = if config.foreground {
        run()
    } else {
        // A panic (in practice, init's) ends the daemon as an error does,
        // through the same flush and rollback below: unwinding on would run
        // the caller's frames in a detached process whose launcher has
        // already read a failure (R166).
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
            .unwrap_or_else(|panic| Err(panicked(&*panic).into()))
    };
    result.map_err(|e| e.with_rollback_failures(abort.roll_back()))
}

/// The steps of [`run_post_fork`], recording in `abort` what each changed.
///
/// # Safety
///
/// As [`run_post_fork`].
#[allow(unsafe_code)]
unsafe fn run_steps<T>(
    config: &DaemonConfig,
    pipe_wr: &mut Option<NotifyChannel>,
    init: impl FnOnce() -> Result<T, DaemonizeError>,
    abort: &mut steps::AbortGuard,
) -> Result<(DaemonContext, T), DaemonizeError> {
    let steps::AbortGuard { ledger, files } = &mut *abort;
    // Step 4: Set umask
    steps::set_umask(config.umask, ledger)?;

    // Step 5: chdir
    steps::change_dir(&config.chdir, ledger)?;

    // Step 6: Redirect stdin to /dev/null (always); redirect stdout/stderr
    // to /dev/null only when not in foreground mode (foreground leaves them
    // inherited so output reaches the terminal or supervisor).
    steps::redirect_to_devnull(!config.foreground, ledger)?;

    // Step 7: Open and lock lockfile (explicit path, or the pidfile itself
    // unless derivation was opted out)
    let lockfile_path = config.effective_lockfile().map(std::path::PathBuf::as_path);
    if let Some(path) = lockfile_path {
        let derived = config.lockfile_is_derived();
        let (lock, created, id) = steps::lock_path(path, derived)?;
        files.push(path, steps::Role::Lockfile(lock), id);
        // A lockfile derived from the pidfile goes with step 8's token. One
        // named apart from it gets its own, even when the pidfile path links
        // to it: step 8 refuses that link, and the file is still this start's.
        if created && !derived {
            let removal = steps::Removal::new(path, id, steps::Created::Lockfile);
            ledger.record_lasting(steps::Saved::Removal(removal))?;
        }
    }

    // Step 8: Write pidfile. The debt is owed from the moment this process
    // owns the file on disk — once `open_pidfile` returns, having locked it at
    // step 7 or created and truncated it just now — not from the moment the
    // write succeeds: a write that fails half way leaves an empty file naming
    // no process at all. Nor any earlier: an `open_pidfile` that fails, or a
    // lock conflict at step 7, leaves a file that belongs to someone else.
    // Its token removes the pidfile from the second half of step 8 on, in
    // both modes: the file outlives a daemon that exits.
    let mut pidfile_id = None;
    if let Some(ref pidfile_path) = config.pidfile {
        let pidfile = steps::open_pidfile(pidfile_path, files.lock())?;
        let id = pidfile.id()?;
        let removal = steps::Removal::new(pidfile_path, id, steps::Created::Pidfile);
        ledger.record_lasting(steps::Saved::Removal(removal))?;
        let fd = pidfile.write_pid()?;
        pidfile_id = Some(id);
        // A standalone pidfile's descriptor, kept for step 16's chown (R163).
        if let Some(fd) = fd {
            files.push(pidfile_path, steps::Role::Pidfile(fd), id);
        }
    }

    // Step 9: Reset signal dispositions
    signals::reset_signal_dispositions(ledger)?;

    // Step 10: Clear signal mask
    steps::clear_signal_mask(ledger)?;

    // Step 11: Set environment variables
    steps::set_env_vars(&config.env, ledger)?;

    // Step 12: Redirect stdout/stderr to configured files. Only a foreground
    // caller sees the slots after a failure, so only it pays for saving them.
    // The files it opened stay held for step 16's chown (R163).
    if config.stdout.is_some() || config.stderr.is_some() {
        let opened = steps::redirect_output(
            config.stdout.as_deref(),
            config.stderr.as_deref(),
            config.append,
            &files.owned(pidfile_id),
            ledger,
        )?;
        for (path, fd, id) in opened {
            files.push(path, steps::Role::Stream(fd), id);
        }
    }

    // Step 13: Close inherited fds (if enabled)
    if config.close_fds {
        let pipe = pipe_wr.as_ref().map(|wr| wr.as_fd().as_raw_fd());
        let spared = steps::spared_fds(files, ledger.held_fds(), pipe);
        steps::close_inherited_fds(&spared)?;
    }

    // Step 14: Resolve the target identity before init, so a user or group
    // that does not exist fails before init's side effects run (R155).
    let identity = if config.user.is_some() || config.group.is_some() {
        Some(ResolvedIdentity::resolve(
            config.user.as_deref(),
            config.group.as_deref(),
        )?)
    } else {
        None
    };
    // The supplementary groups too, from the real group database, before
    // init can change root (R162).
    if let Some(identity) = &identity {
        identity.join_groups(ledger)?;
    }

    // Step 15: Empty the stream files, the one change no rollback can undo,
    // so it comes as late as `init` allows: init writes to them.
    if !config.append {
        steps::truncate_streams(files.streams(), ledger)?;
    }

    // The caller's privileged init (R153). After the lock, so only
    // the instance holding it runs; after the fd sweep, so what it opens
    // stays open. An error is the sequence's, and the abort guard removes the
    // pidfile (R154). From here other threads may run, and `setenv` with one
    // alive is undefined behaviour, so a failure leaves the env as step 11
    // set it.
    // A change between the last token and here, not undone, fails the walk.
    ledger.checkpoint()?;
    ledger.threads_may_start();
    let value = init()?;

    // Steps 16-17: Hand the daemon's files to the target identity while still
    // privileged, then switch to it (R155).
    if let Some(identity) = &identity {
        if config.chown_paths {
            files.chown_to(identity.chown_ids(), ledger)?;
        }
    }
    #[cfg(test)]
    if steps::failpoints::BEFORE_SWITCH_FAILS.injected() {
        return Err(ErrorKind::SystemError(steps::failpoints::SWITCH_FAILURE.into()).into());
    }
    if let Some(identity) = identity {
        // SAFETY: this function's contract requires a single thread, and the
        // checked entry points verify it again after init (R126).
        unsafe { identity.switch_to() }?;
    }

    // Step 18: Return DaemonContext (clones the config-derived fields it needs).
    // The context owns the pidfile and the lock from here. A launcher waits
    // until the daemon reports readiness, so the start can still fail: the
    // context takes what the start changed, to undo on that failure (R164).
    // In foreground mode the start has succeeded, so the guard stands down.
    // The lock stays in the guard while a launcher waits: step 16 may have
    // chowned it, and a failure gives it back (R170).
    let (lockfile, launcher) = match pipe_wr.take() {
        Some(pipe) => {
            let start = std::mem::replace(abort, steps::AbortGuard::new(false));
            (None, Some((pipe, start)))
        }
        None => {
            abort.keep();
            (abort.files.take_lock(), None)
        }
    };
    let pidfile = config
        .pidfile
        .as_deref()
        .zip(pidfile_id)
        .map(|(path, id)| steps::Removal::new(path, id, steps::Created::Pidfile));
    Ok((
        DaemonContext::new(config, pidfile, lockfile, launcher),
        value,
    ))
}

/// Parent-side channel reader. Reads from the channel and exits accordingly.
fn parent_pipe_reader(rd: OwnedFd, forker: &impl Forker) -> ! {
    match notify::read_outcome(&mut std::fs::File::from(rd)) {
        notify::Outcome::Success => forker.exit(0),
        notify::Outcome::Failure { code, message } => {
            notify::print_on_stderr(message);
            forker.exit(code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    use crate::forker::null_forker::NullForker;
    use crate::test_support::isolated;
    use std::panic::catch_unwind;

    /// Test wrapper: `daemonize_inner` driven by the non-forking `NullForker`.
    ///
    /// `NullForker::fork` returns a configured result without actually forking,
    /// so the `unsafe fn`'s single-threaded contract is vacuously satisfied —
    /// keeping the call sites free of per-test `unsafe` blocks.
    #[allow(unsafe_code)]
    fn run_inner(
        config: &DaemonConfig,
        forker: &mut NullForker,
    ) -> Result<DaemonContext, DaemonizeError> {
        // SAFETY: NullForker does not fork.
        unsafe { daemonize_inner(config, forker) }
    }

    /// As [`run_inner`], with privileged `init`.
    #[allow(unsafe_code)]
    fn run_inner_with<T>(
        config: &DaemonConfig,
        forker: &mut NullForker,
        init: impl FnOnce() -> Result<T, DaemonizeError>,
    ) -> Result<(DaemonContext, T), DaemonizeError> {
        // SAFETY: NullForker does not fork.
        unsafe { daemonize_inner_with(config, forker, init) }
    }

    // Covers: R153
    #[test]
    fn init_runs_only_in_the_instance_holding_the_lock() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let _held = steps::open_and_lock(&pidfile).expect("the running instance's lock");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut ran = false;

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
            ran = true;
            Ok(())
        });

        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::LockConflict { .. })
            ),
            "{result:?}"
        );
        assert!(!ran, "init ran in the instance that lost the lock");
    }

    // Covers: R153
    #[test]
    fn init_opens_after_the_fd_sweep_and_hands_back_its_value() {
        use std::io::{Read, Seek, Write};
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(true);

        let (_ctx, mut file) =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Ok(tempfile::tempfile_in(dir.path()).unwrap())
            })
            .expect("the sequence succeeds");

        file.write_all(b"alive")
            .expect("init's file survived the fd sweep");
        file.rewind().unwrap();
        let mut back = String::new();
        file.read_to_string(&mut back).unwrap();
        assert_eq!(back, "alive");
    }

    // Covers: R154
    #[test]
    fn init_error_returns_its_code_and_removes_the_pidfile() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "database unreachable"))
            });

        let err = result.expect_err("init's error is the sequence's error");
        assert_eq!(err.exit_code(), 75);
        assert!(!pidfile.exists(), "a failed init left the pidfile behind");
    }

    // Covers: R145, R154
    #[test]
    fn a_failed_start_removes_the_pidfile_even_without_cleanup_on_drop() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .pidfile(&pidfile)
            .cleanup_on_drop(false);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "database unreachable"))
            });

        assert!(result.is_err());
        assert!(!pidfile.exists(), "a failed start left its pidfile");
    }

    use crate::test_support::signal_state;

    extern "C" fn caller_handler(_: libc::c_int) {}

    // Covers: R164
    /// The caller's handlers (the Rust runtime's stack-overflow SIGSEGV one
    /// among them) and its mask come back when the start fails after steps 9
    /// and 10 changed them, even from init, as sigaction and sigprocmask are
    /// safe with other threads running.
    #[test]
    #[allow(unsafe_code)]
    fn a_failed_foreground_start_restores_signals() {
        use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
        if !isolated() {
            return;
        }
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
        let before = signal_state();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "database unreachable"))
            });

        assert!(result.is_err());
        assert_eq!(signal_state(), before, "signal state not restored");
    }

    // Covers: R164
    #[test]
    #[allow(unsafe_code)]
    #[allow(clippy::disallowed_methods)] // setenv thread safety: a one-thread subprocess
    fn a_start_failing_before_init_restores_the_env() {
        if !isolated() {
            return;
        }
        // SAFETY: this subprocess runs one thread.
        unsafe { std::env::set_var("BLIVET_KEPT", "caller") };
        // SAFETY: as above.
        unsafe { std::env::remove_var("BLIVET_NEW") };
        let dir = tempfile::tempdir().unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout(dir.path().join("app.log"))
            .env("BLIVET_KEPT", "daemon")
            .env("BLIVET_NEW", "daemon");
        steps::failpoints::STREAM_TRUNCATE_FAILS.set();

        let result = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::OutputFileError(_))
            ),
            "{result:?}"
        );
        assert_eq!(std::env::var("BLIVET_KEPT").unwrap(), "caller");
        assert!(
            std::env::var_os("BLIVET_NEW").is_none(),
            "a set variable stayed"
        );
    }

    // Covers: R164
    /// From init on, other threads may run, and `setenv` with another thread
    /// alive is undefined behaviour. The env stays as step 11 set it.
    #[test]
    fn a_start_failing_in_init_leaves_the_env_alone() {
        if !isolated() {
            return;
        }
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .env("BLIVET_SET", "daemon");

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "database unreachable"))
            });

        assert!(result.is_err());
        assert_eq!(
            std::env::var("BLIVET_SET").as_deref(),
            Ok("daemon"),
            "the env was restored after init, when threads may run"
        );
    }

    // Covers: R164
    #[test]
    #[ignore = "needs root: the privileged tier runs it"]
    fn root_gets_its_groups_back_after_a_failed_foreground_start() {
        if !isolated() {
            return;
        }
        crate::sys::raw_setgroups(&[0, 4242, 4343]).unwrap();
        let before = crate::test_support::supplementary_gids();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).user("nobody");

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                assert_ne!(crate::test_support::supplementary_gids(), before);
                Err(DaemonizeError::application(75, "database unreachable"))
            });

        assert!(result.is_err());
        assert_eq!(crate::test_support::supplementary_gids(), before);
    }

    // Covers: R64, R65b, R155
    #[test]
    fn identity_chown_runs_after_init_and_before_the_switch() {
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .pidfile(&pidfile)
            .group(crate::test_support::foreign_gid().to_string());
        let mut ran = false;

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
            ran = true;
            Ok(())
        });

        assert!(ran, "init did not run before the chown");
        assert!(
            matches!(result.as_ref().map_err(DaemonizeError::kind), Err(ErrorKind::ChownError(m)) if m.contains("app.pid")),
            "the pidfile is chowned before the switch: {result:?}"
        );
    }

    // Covers: R65a, R125
    #[test]
    fn chown_paths_false_goes_straight_to_the_switch() {
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let dir = tempfile::tempdir().unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .pidfile(dir.path().join("app.pid"))
            .group(crate::test_support::foreign_gid().to_string())
            .chown_paths(false);

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || Ok(()));

        assert!(
            matches!(result.as_ref().map_err(DaemonizeError::kind), Err(ErrorKind::PermissionDenied(m)) if m.contains("setgid")),
            "with chown_paths(false) the unprivileged failure is setgid's: {result:?}"
        );
    }

    // Covers: R163
    /// `-o /dev/null` is a common way to drop output. The device is not the
    /// daemon's file: chowning it would hand `/dev/null` to the daemon's user
    /// as root, and as anyone else it fails the start with `ChownError`. The
    /// unprivileged run sees the second; the switch then fails on its own.
    #[test]
    fn output_to_a_device_is_not_chowned() {
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout("/dev/null")
            .stderr("/dev/null")
            .group(crate::test_support::foreign_gid().to_string());

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || Ok(()));

        assert!(
            matches!(result.as_ref().map_err(DaemonizeError::kind), Err(ErrorKind::PermissionDenied(m)) if m.contains("setgid")),
            "the device was chowned: {result:?}"
        );
    }

    // Covers: R163
    #[test]
    #[ignore = "needs root: the privileged tier runs it"]
    fn root_leaves_a_device_output_with_its_owner() {
        use std::os::unix::fs::MetadataExt;
        if !isolated() {
            return;
        }
        let owner = |m: std::fs::Metadata| (m.uid(), m.gid());
        let before = owner(std::fs::metadata("/dev/null").unwrap());
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout("/dev/null")
            .stderr("/dev/null")
            .user("nobody");

        let _ = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || Ok(()));

        assert_eq!(owner(std::fs::metadata("/dev/null").unwrap()), before);
    }

    // Covers: R164
    /// The chown needs no descriptor of its own: an init that leaves none
    /// free does not fail it.
    #[test]
    fn a_full_fd_table_does_not_fail_the_chown() {
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let dir = tempfile::tempdir().unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout(dir.path().join("app.log"))
            .group(nix::unistd::getgid().to_string());

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
            std::mem::forget(crate::test_support::fill_fd_table(0));
            Ok(())
        });

        assert!(result.is_ok(), "{:?}", result.err());
    }

    // Covers: R164
    /// The rollback gives an owner back by the descriptor the start holds, so
    /// an init that leaves none free costs it nothing. A log it keeps,
    /// written to, is no rollback failure.
    #[test]
    #[allow(clippy::disallowed_methods)] // init writes to fd 1, in a subprocess of its own
    fn an_owner_is_given_back_with_no_descriptor_free() {
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let dir = tempfile::tempdir().unwrap();
        // BSD gives a new file its directory's group, which the caller may
        // not be in, and so could not give back.
        nix::unistd::chown(dir.path(), None, Some(nix::unistd::getgid())).unwrap();
        let log = dir.path().join("app.log");
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout(&log)
            .group(nix::unistd::getgid().to_string());
        steps::failpoints::BEFORE_SWITCH_FAILS.set();

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                std::mem::forget(crate::test_support::fill_fd_table(0));
                nix::unistd::write(std::io::stdout(), b"started\n").unwrap();
                Ok(())
            });

        let err = result.expect_err("the failpoint failed the start");
        assert!(log.exists(), "the log written to was removed");
        assert!(
            err.rollback_failures().is_empty(),
            "{:?}",
            err.rollback_failures()
        );
    }

    // Covers: R164
    /// A file another process put at the path since is not the one step 16
    /// chowned: the rollback goes by descriptor and leaves its owner alone.
    #[test]
    #[ignore = "needs root: the privileged tier runs it"]
    fn root_leaves_the_owner_of_a_file_put_at_the_path_since() {
        use std::os::unix::fs::MetadataExt;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let other_gid = crate::test_support::foreign_gid() + 1;
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout(&log)
            .group(crate::test_support::foreign_gid().to_string());
        steps::failpoints::BEFORE_SWITCH_FAILS.set();

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                std::fs::rename(&log, dir.path().join("moved.log")).unwrap();
                std::fs::write(&log, "another process's\n").unwrap();
                nix::unistd::chown(&log, None, Some(nix::unistd::Gid::from_raw(other_gid)))
                    .unwrap();
                std::mem::forget(crate::test_support::fill_fd_table(0));
                Ok(())
            });

        let err = result.expect_err("the failpoint failed the start");
        assert!(
            err.rollback_failures().is_empty(),
            "{:?}",
            err.rollback_failures()
        );
        assert_eq!(std::fs::metadata(&log).unwrap().gid(), other_gid);
    }

    // Covers: R164
    /// The same, as root, where the owner really changes: the file gets its
    /// owner back.
    #[test]
    #[ignore = "needs root: the privileged tier runs it"]
    #[allow(clippy::disallowed_methods)] // init writes to fd 1, in a subprocess of its own
    fn root_gives_a_file_its_owner_back_with_no_descriptor_free() {
        use std::os::unix::fs::MetadataExt;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "").unwrap();
        let before = std::fs::metadata(&log).unwrap().gid();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .append(true)
            .stdout(&log)
            .group(crate::test_support::foreign_gid().to_string());
        steps::failpoints::BEFORE_SWITCH_FAILS.set();

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                std::mem::forget(crate::test_support::fill_fd_table(0));
                Ok(())
            });

        let err = result.expect_err("the failpoint failed the start");
        assert!(
            err.rollback_failures().is_empty(),
            "{:?}",
            err.rollback_failures()
        );
        assert_eq!(std::fs::metadata(&log).unwrap().gid(), before);
    }

    // Covers: R163, R164
    /// A start that succeeds keeps its files chowned: only a failure gives
    /// the owners back.
    #[test]
    #[ignore = "needs root: the privileged tier runs it"]
    fn root_a_started_daemon_keeps_its_files_chowned() {
        use std::os::unix::fs::MetadataExt;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let gid = crate::test_support::foreign_gid();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout(&log)
            .group(gid.to_string());

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || Ok(()));

        assert!(result.is_ok(), "{:?}", result.err());
        assert_eq!(std::fs::metadata(&log).unwrap().gid(), gid);
    }

    // Covers: R163, R164, R170
    /// A daemon whose context drops before readiness never started: the
    /// file step 16 chowned gets its owner back, as on an init failure.
    #[test]
    #[ignore = "needs root: the privileged tier runs it"]
    fn root_a_daemon_dropped_before_readiness_gives_each_owner_back() {
        use std::os::unix::fs::MetadataExt;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "earlier run\n").unwrap();
        let gid_was = std::fs::metadata(&log).unwrap().gid();
        let mut config = DaemonConfig::new();
        config
            .close_fds(false)
            .stdout(&log)
            .group(crate::test_support::foreign_gid().to_string());
        let mut forker = NullForker::both_child_with_reader();

        let (ctx, ()) = run_inner_with(&config, &mut forker, || Ok(())).unwrap();
        drop(ctx);

        assert_eq!(std::fs::metadata(&log).unwrap().gid(), gid_was);
    }

    // Covers: R163, R170
    /// The lock is one of the files step 16 chowns: a daemon dropped before
    /// readiness gives a lockfile it found its owner back too.
    #[test]
    #[ignore = "needs root: the privileged tier runs it"]
    fn root_a_daemon_dropped_before_readiness_gives_the_lockfile_its_owner_back() {
        use std::os::unix::fs::MetadataExt;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let lockfile = dir.path().join("app.lock");
        std::fs::write(&lockfile, "").unwrap();
        let gid_was = std::fs::metadata(&lockfile).unwrap().gid();
        let mut config = DaemonConfig::new();
        config
            .close_fds(false)
            .lockfile(&lockfile)
            .group(crate::test_support::foreign_gid().to_string());
        let mut forker = NullForker::both_child_with_reader();

        let (ctx, ()) = run_inner_with(&config, &mut forker, || Ok(())).unwrap();
        drop(ctx);

        assert_eq!(std::fs::metadata(&lockfile).unwrap().gid(), gid_was);
    }

    // Covers: R163
    /// An init that puts another file on fd 1 must not have that file
    /// chowned: the chown reaches the stream step 12 opened. Here that is a
    /// device, which is skipped, so the unprivileged failure is setgid's; a
    /// chown through fd 1 would fail on the regular file first.
    #[test]
    #[allow(clippy::disallowed_methods)] // init moves fd 1, in a subprocess of its own
    fn the_chown_follows_the_opened_output_not_fd_1() {
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let other = tempfile::tempfile().unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout("/dev/null")
            .stderr("/dev/null")
            .group(crate::test_support::foreign_gid().to_string());

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
            nix::unistd::dup2_stdout(&other).unwrap();
            nix::unistd::dup2_stderr(&other).unwrap();
            Ok(())
        });

        assert!(
            matches!(result.as_ref().map_err(DaemonizeError::kind), Err(ErrorKind::PermissionDenied(m)) if m.contains("setgid")),
            "the file init put on fd 1 was chowned: {result:?}"
        );
    }

    // Covers: R163
    #[test]
    #[ignore = "needs root: the privileged tier runs it"]
    #[allow(clippy::disallowed_methods)] // init moves fd 1, in a subprocess of its own
    fn root_chowns_the_opened_output_not_what_init_put_on_fd_1() {
        use std::os::unix::fs::MetadataExt;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (log, secret) = (dir.path().join("app.log"), dir.path().join("secret"));
        let secret_file = std::fs::File::create(&secret).unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout(&log)
            .user("nobody");

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
            nix::unistd::dup2_stdout(&secret_file).unwrap();
            Ok(())
        });

        result.expect("the start succeeds");
        let nobody = nix::unistd::User::from_name("nobody").unwrap().unwrap();
        assert_eq!(std::fs::metadata(&log).unwrap().uid(), nobody.uid.as_raw());
        assert_eq!(
            std::fs::metadata(&secret).unwrap().uid(),
            0,
            "init's file was chowned"
        );
    }

    /// Ignored for the reason the `identity::tests` lookups of missing names
    /// are: NSS can block on a networked source. The glibc container tier runs
    /// it with `--include-ignored`.
    // Covers: R155
    #[test]
    #[ignore = "getpwnam for a missing user can block on a networked NSS source"]
    fn unknown_user_fails_before_init_runs() {
        if !isolated() {
            return;
        }
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .user("nonexistent_daemonize_test_user_xyz");
        let mut ran = false;

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
            ran = true;
            Ok(())
        });

        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::UserNotFound(_))
            ),
            "{result:?}"
        );
        assert!(!ran, "init ran for a user that does not exist");
    }

    // Covers: R163
    /// An init that takes the pidfile's path away (a chroot does, in effect)
    /// must not make the chown skip it: the chown reaches the file the
    /// sequence opened. Unprivileged, that chown fails, which shows it ran.
    #[test]
    fn the_chown_follows_the_opened_pidfile_not_its_path() {
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(true)
            .pidfile(&pidfile)
            .no_lockfile()
            .group(crate::test_support::foreign_gid().to_string());

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
            std::fs::remove_file(&pidfile).unwrap();
            Ok(())
        });

        assert!(
            matches!(result.as_ref().map_err(DaemonizeError::kind), Err(ErrorKind::ChownError(m)) if m.contains("app.pid")),
            "the opened pidfile was not chowned: {result:?}"
        );
    }

    // Covers: R97
    /// Step 8 is handed the descriptor step 7 locked, so it opens no second
    /// one: while init runs, one descriptor holds the pidfile. That the write
    /// then goes through that descriptor, not the path, is pinned at the step:
    /// `steps::pidfile`'s `write_pidfile_shared_writes_through_locked_fd_not_path`.
    /// The fd scan is a raw `fstat` loop because `list_open_fds` also lists
    /// its own, already closed, directory descriptor, which no `BorrowedFd`
    /// may name.
    #[test]
    #[allow(unsafe_code)]
    fn a_derived_lock_pidfile_is_written_through_the_lock() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);

        let (_ctx, holding) = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
            let id = crate::file_identity::FileId::of(&nix::sys::stat::stat(&pidfile).unwrap());
            let holding = (0..256)
                .filter(|&fd| {
                    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
                    // SAFETY: `st` is valid for `fstat` to fill; a closed
                    // `fd` returns EBADF without touching it.
                    if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
                        return false;
                    }
                    // SAFETY: `fstat` succeeded, so it filled `st`.
                    let st = unsafe { st.assume_init() };
                    (st.st_dev, st.st_ino) == (id.dev, id.ino)
                })
                .count();
            Ok(holding)
        })
        .unwrap();

        assert_eq!(holding, 1);
    }

    // Covers: R162
    /// Unprivileged, `initgroups` fails with EPERM, which shows where it runs:
    /// the error must come before init, not after it.
    #[test]
    fn supplementary_groups_are_set_before_init_runs() {
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).user("0");
        let mut ran = false;

        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
            ran = true;
            Ok(())
        });

        assert!(
            matches!(result.as_ref().map_err(DaemonizeError::kind), Err(ErrorKind::PermissionDenied(m)) if m.contains("initgroups")),
            "{result:?}"
        );
        assert!(!ran, "init ran before the supplementary groups were set");
    }

    // Covers: R164
    /// An init that sheds root leaves the rollback unable to put the caller's
    /// supplementary groups back, and the error says so.
    #[test]
    #[ignore = "needs root: the privileged tier runs it"]
    fn supplementary_groups_the_rollback_cannot_restore_are_reported() {
        use crate::ChangedState;
        if !isolated() {
            return;
        }
        std::env::set_current_dir("/").unwrap();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).user("nobody");

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                crate::test_support::shed_root();
                Err(DaemonizeError::application(75, "init failed"))
            });

        let err = result.expect_err("init failed the start");
        let report: Vec<_> = err
            .rollback_failures()
            .iter()
            .map(|f| (f.state(), f.error().raw_os_error()))
            .collect();
        assert_eq!(
            report,
            [(ChangedState::SupplementaryGroups, Some(libc::EPERM))]
        );
    }

    // Covers: R162
    /// An init that chroots into a jail whose `/etc/group` lists no groups
    /// must not cost the daemon its supplementary groups (ADR 0005).
    #[test]
    #[ignore = "needs root and the image's testuser: the privileged tier runs it"]
    fn supplementary_groups_come_from_the_real_database_when_init_chroots() {
        if !isolated() {
            return;
        }
        let supp = nix::unistd::Group::from_name("testsupp")
            .unwrap()
            .expect("the privileged image has testsupp")
            .gid;
        let jail = tempfile::tempdir().unwrap();
        std::fs::create_dir(jail.path().join("etc")).unwrap();
        std::fs::write(jail.path().join("etc/group"), "").unwrap();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).user("testuser");

        let root = jail.path().to_path_buf();
        let result = run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
            nix::unistd::chroot(&root).unwrap();
            nix::unistd::chdir("/").unwrap();
            Ok(())
        });

        result.expect("the sequence succeeds");
        let groups = crate::test_support::supplementary_gids();
        assert!(
            groups.contains(&supp.as_raw()),
            "testsupp ({supp}) missing from {groups:?}: groups were read inside the jail"
        );
    }

    // Covers: R164
    /// `setgid` succeeded and `setuid` failed: the foreground caller, still
    /// root, gets its primary group back rather than the target user's.
    #[test]
    #[ignore = "needs root and the image's testuser: the privileged tier runs it"]
    fn a_failed_setuid_gives_the_caller_its_group_back() {
        if !isolated() {
            return;
        }
        let before = (nix::unistd::getgid(), nix::unistd::getegid());
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).user("testuser");
        steps::failpoints::SETUID_FAILS.set();

        let result = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        assert!(
            matches!(result.as_ref().map_err(DaemonizeError::kind), Err(ErrorKind::PermissionDenied(m)) if m.contains("setuid")),
            "{result:?}"
        );
        assert_eq!(
            (nix::unistd::getgid(), nix::unistd::getegid()),
            before,
            "the target user's group was left in place"
        );
    }

    // Covers: R164
    /// Foreground mode changes the caller's own process. A failure after
    /// step 12 (here, init's) must leave stdin, stdout, stderr, the umask, and
    /// the working directory as the caller had them, so the error it prints
    /// reaches its terminal, not the log file.
    #[test]
    fn a_failed_foreground_sequence_puts_the_callers_process_back() {
        use nix::sys::stat::{Mode, fstat, umask};
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let id = |fd: std::os::fd::BorrowedFd<'_>| {
            let st = fstat(fd).unwrap();
            (st.st_dev, st.st_ino)
        };
        let stdio = || {
            (
                id(std::io::stdin().as_fd()),
                id(std::io::stdout().as_fd()),
                id(std::io::stderr().as_fd()),
            )
        };
        let before = stdio();
        let cwd_before = std::env::current_dir().unwrap();
        umask(Mode::from_bits_truncate(crate::test_support::CALLER_UMASK));
        let umask_before = crate::test_support::current_umask();

        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .umask(0o077)
            .chdir(work.path())
            .pidfile(dir.path().join("app.pid"))
            .stdout(dir.path().join("out.log"))
            .stderr(dir.path().join("err.log"));
        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "init failed"))
            });

        let umask_after = umask(umask_before);
        assert!(result.is_err());
        assert_eq!(stdio(), before, "stdio was left redirected");
        assert_eq!(
            std::env::current_dir().unwrap(),
            cwd_before,
            "cwd was left changed"
        );
        assert_eq!(umask_after, umask_before, "umask was left changed");
        assert!(
            !dir.path().join("app.pid").exists(),
            "pidfile was left behind"
        );
    }

    // Covers: R154, R164
    /// A change the rollback cannot put back goes in the error beside init's
    /// own, which keeps its message. Here init swaps the file under the
    /// descriptor the working-directory token holds for `/dev/null`, so its
    /// `fchdir` fails.
    #[test]
    fn a_failed_restore_is_reported_beside_inits_error() {
        use crate::ChangedState;
        if !isolated() {
            return;
        }
        let start = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::env::set_current_dir(start.path()).unwrap();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).chdir(work.path());

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                crate::test_support::swap_out_fds_open_on(start.path());
                Err(DaemonizeError::application(75, "init failed"))
            });

        let err = result.expect_err("init failed the start");
        let report: Vec<_> = err
            .rollback_failures()
            .iter()
            .map(|f| (f.state(), f.error().raw_os_error()))
            .collect();
        assert_eq!(
            (err.to_string().as_str(), report),
            (
                "application error: init failed",
                vec![(ChangedState::WorkingDirectory, Some(libc::ENOTDIR))]
            )
        );
    }

    // Covers: R164
    /// A working directory the caller may enter but not read is still put
    /// back: saving it needs search permission only.
    #[test]
    #[cfg(blivet_search_only_open)]
    fn an_unreadable_working_directory_is_put_back() {
        use std::os::unix::fs::PermissionsExt;
        if !isolated() {
            return;
        }
        let start = tempfile::tempdir().unwrap();
        std::fs::set_permissions(start.path(), std::fs::Permissions::from_mode(0o311)).unwrap();
        crate::test_support::shed_root();
        std::env::set_current_dir(start.path()).unwrap();
        let work = tempfile::tempdir().unwrap();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).chdir(work.path());

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "init failed"))
            });

        let err = result.expect_err("init failed the start");
        assert_eq!(
            (
                err.rollback_failures().len(),
                std::env::current_dir().unwrap()
            ),
            (0, start.path().canonicalize().unwrap())
        );
    }

    // Covers: R164
    /// A working directory that lost its search permission after the caller
    /// entered it cannot be saved, so the rollback cannot go back to it, and
    /// says so with the reason.
    #[test]
    fn a_working_directory_that_could_not_be_saved_is_reported() {
        use crate::ChangedState;
        use std::os::unix::fs::PermissionsExt;
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let start = tempfile::tempdir().unwrap();
        std::env::set_current_dir(start.path()).unwrap();
        std::fs::set_permissions(start.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "init failed"))
            });

        let err = result.expect_err("init failed the start");
        let report: Vec<_> = err
            .rollback_failures()
            .iter()
            .map(|f| (f.state(), f.error().raw_os_error()))
            .collect();
        assert_eq!(
            report,
            [(ChangedState::WorkingDirectory, Some(libc::EACCES))]
        );
    }

    // Covers: R164
    /// A pidfile in a directory init made read-only stays, and the rollback
    /// says so with the path and the reason.
    #[test]
    fn a_pidfile_the_rollback_cannot_remove_is_reported() {
        use crate::ChangedState;
        use std::os::unix::fs::PermissionsExt;
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let read_only = |mode| {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        };

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                read_only(0o555);
                Err(DaemonizeError::application(75, "init failed"))
            });
        read_only(0o755);

        let err = result.expect_err("init failed the start");
        let report: Vec<_> = err
            .rollback_failures()
            .iter()
            .map(|f| (f.state(), f.path(), f.error().raw_os_error()))
            .collect();
        assert_eq!(
            report,
            [(ChangedState::Pidfile, Some(&*pidfile), Some(libc::EACCES))]
        );
    }

    // Covers: R164
    /// Latest first: the pidfile (step 8) before the working directory
    /// (step 5).
    #[test]
    fn rollback_failures_come_latest_step_first() {
        use crate::ChangedState;
        use std::os::unix::fs::PermissionsExt;
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let start = tempfile::tempdir().unwrap();
        std::env::set_current_dir(start.path()).unwrap();
        std::fs::set_permissions(start.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let read_only = |mode| {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        };

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                read_only(0o555);
                Err(DaemonizeError::application(75, "init failed"))
            });
        read_only(0o755);

        let err = result.expect_err("init failed the start");
        let states: Vec<_> = err.rollback_failures().iter().map(|f| f.state()).collect();
        assert_eq!(
            states,
            [ChangedState::Pidfile, ChangedState::WorkingDirectory]
        );
    }

    // Covers: R164
    /// A panic in a foreground init unwinds into the caller, past the error
    /// path: the guard's drop still rolls back and removes the pidfile.
    #[test]
    fn a_panicking_foreground_init_still_rolls_back() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);

        let unwound = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner_with(
                &config,
                &mut NullForker::new(vec![], Ok(())),
                || -> Result<(), _> { panic!("init panicked") },
            )
        }));

        assert!(unwound.is_err(), "init's panic reached the caller");
        assert!(!pidfile.exists(), "pidfile was left behind");
    }

    // Covers: R164
    #[test]
    fn a_pidfile_already_gone_is_not_a_rollback_failure() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                std::fs::remove_file(&pidfile).unwrap();
                Err(DaemonizeError::application(75, "init failed"))
            });

        let err = result.expect_err("init failed the start");
        assert!(
            err.rollback_failures().is_empty(),
            "{:?}",
            err.rollback_failures()
        );
    }

    // Covers: R164
    /// The abort guard removes the pidfile and then releases the lock, so a
    /// second instance never takes a lock whose pidfile is about to vanish.
    /// The order is the guard's `Drop`; this checks that both happen.
    #[test]
    fn a_failed_start_frees_the_lock_for_the_next_one() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .pidfile(&pidfile)
            .lockfile(dir.path().join("app.lock"));
        let failed: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "init failed"))
            });
        assert!(failed.is_err());
        assert!(!pidfile.exists(), "pidfile was left behind");

        let again = run_inner(&config, &mut NullForker::new(vec![], Ok(())));
        assert!(again.is_ok(), "lock was left held: {again:?}");
    }

    // Covers: R164
    /// `sudo -u app daemonize -f` from a home the user may enter but not list:
    /// saving the working directory for undo must not fail the start.
    #[test]
    fn a_foreground_start_from_a_directory_it_cannot_read_succeeds() {
        use std::os::unix::fs::PermissionsExt;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let search_only = tempfile::tempdir().unwrap();
        std::fs::set_permissions(search_only.path(), std::fs::Permissions::from_mode(0o111))
            .unwrap();
        std::env::set_current_dir(search_only.path()).unwrap();

        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .pidfile(dir.path().join("app.pid"));
        let result = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        std::fs::set_permissions(search_only.path(), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        result.unwrap();
    }

    // Covers: R164
    /// A daemon's process exits on failure, but the files it created would
    /// outlive it: they go in daemon mode too.
    #[test]
    fn a_failed_daemon_leaves_no_file_it_created() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut config = DaemonConfig::new();
        config
            .close_fds(false)
            .pidfile(dir.path().join("app.pid"))
            .lockfile(dir.path().join("app.lock"))
            .stdout(dir.path().join("out.log"))
            .stderr(dir.path().join("err.log"));
        let mut forker = NullForker::both_child();

        let exited = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner_with(&config, &mut forker, || -> Result<(), _> {
                Err(DaemonizeError::application(75, "database unreachable"))
            })
        }));

        assert!(exited.is_err(), "a daemon-mode failure exits");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    // Covers: R164
    /// A log the start created but its daemon wrote to holds what went
    /// wrong: the failure keeps it.
    #[test]
    fn a_failed_daemon_keeps_a_log_it_wrote_to() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("err.log");
        let mut config = DaemonConfig::new();
        config.close_fds(false).stderr(&log);
        let mut forker = NullForker::both_child();

        let exited = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner_with(&config, &mut forker, || -> Result<(), _> {
                std::io::Write::write_all(&mut std::io::stderr(), b"connect: refused\n").unwrap();
                Err(DaemonizeError::application(75, "database unreachable"))
            })
        }));

        assert!(exited.is_err(), "a daemon-mode failure exits");
        let kept = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(kept.starts_with("connect: refused\n"), "log: {kept:?}");
    }

    // Covers: R164
    /// A symlink put at the pidfile's path since, even one to the pidfile,
    /// is not the start's to remove: the rollback unlinks a path only while
    /// the path itself is the file the start wrote.
    #[test]
    fn a_symlink_put_at_the_pidfile_path_is_left() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let moved = dir.path().join("moved.pid");
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .no_lockfile()
            .pidfile(&pidfile);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                std::fs::rename(&pidfile, &moved).unwrap();
                std::os::unix::fs::symlink(&moved, &pidfile).unwrap();
                Err(DaemonizeError::application(75, "init failed"))
            });

        assert!(result.is_err());
        assert!(
            std::fs::symlink_metadata(&pidfile).is_ok(),
            "the rollback removed a symlink it did not make"
        );
    }

    // Covers: R164
    /// A pidfile path that is a symlink is refused, not written through: a
    /// failed start could not then remove what it wrote. Opened standalone.
    #[test]
    fn a_standalone_pidfile_symlink_is_refused() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.pid");
        std::fs::write(&target, "").unwrap();
        let pidfile = dir.path().join("app.pid");
        std::os::unix::fs::symlink(&target, &pidfile).unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .no_lockfile()
            .pidfile(&pidfile);

        let result = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::PidfileError(_))
            ),
            "{result:?}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
    }

    // Covers: R164
    /// The same, for a pidfile that is its own lockfile.
    #[test]
    fn a_locked_pidfile_symlink_is_refused() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.pid");
        std::fs::write(&target, "").unwrap();
        let pidfile = dir.path().join("app.pid");
        std::os::unix::fs::symlink(&target, &pidfile).unwrap();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);

        let result = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::LockfileError(_))
            ),
            "{result:?}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
    }

    // Covers: R164
    /// A pidfile symlink is refused when an explicit lockfile names its
    /// target too: the pidfile path is still a link the rollback could not
    /// remove.
    #[test]
    fn a_pidfile_symlink_to_its_explicit_lockfile_is_refused() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let lockfile = dir.path().join("app.lock");
        let pidfile = dir.path().join("app.pid");
        std::os::unix::fs::symlink(&lockfile, &pidfile).unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .pidfile(&pidfile)
            .lockfile(&lockfile);

        let result = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::PidfileError(_))
            ),
            "{result:?}"
        );
    }

    // Covers: R164
    /// The lockfile that start created is still its own to remove.
    #[test]
    fn the_lockfile_created_for_a_refused_pidfile_symlink_is_removed() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let lockfile = dir.path().join("app.lock");
        let pidfile = dir.path().join("app.pid");
        std::os::unix::fs::symlink(&lockfile, &pidfile).unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .pidfile(&pidfile)
            .lockfile(&lockfile);

        let _ = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        assert!(!lockfile.exists(), "the created lockfile was left");
    }

    // Covers: R164
    /// The refusal names the cause, not the platform's errno for it.
    #[test]
    fn a_refused_pidfile_symlink_is_named_as_one() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.pid");
        let pidfile = dir.path().join("app.pid");
        std::os::unix::fs::symlink(&target, &pidfile).unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .no_lockfile()
            .pidfile(&pidfile);

        let err = run_inner(&config, &mut NullForker::new(vec![], Ok(()))).unwrap_err();

        assert!(err.to_string().contains("is a symlink"), "{err}");
    }

    // Covers: R164
    /// An explicit lockfile may be a symlink, even to the pidfile: only the
    /// pidfile path, and a lockfile derived from it, refuse one.
    #[test]
    fn an_explicit_lockfile_symlink_to_the_pidfile_is_followed() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        std::fs::write(&pidfile, "").unwrap();
        let lockfile = dir.path().join("app.lock");
        std::os::unix::fs::symlink(&pidfile, &lockfile).unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .pidfile(&pidfile)
            .lockfile(&lockfile);

        let result = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        assert!(result.is_ok(), "{result:?}");
    }

    // Covers: R164
    /// A lockfile that was there before the start is not the start's to
    /// remove.
    #[test]
    fn a_failed_start_keeps_a_lockfile_it_found() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let lockfile = dir.path().join("app.lock");
        std::fs::write(&lockfile, "").unwrap();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).lockfile(&lockfile);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "init failed"))
            });

        assert!(result.is_err());
        assert!(lockfile.exists(), "the found lockfile was removed");
    }

    // Covers: R164
    /// A pidfile another process wrote over the start's, under the same name,
    /// is that process's: the rollback leaves it and reports nothing.
    #[test]
    fn a_failed_start_keeps_a_pidfile_put_in_its_place() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                std::fs::remove_file(&pidfile).unwrap();
                std::fs::write(&pidfile, "4242\n").unwrap();
                Err(DaemonizeError::application(75, "init failed"))
            });

        let err = result.expect_err("init failed the start");
        assert!(err.rollback_failures().is_empty());
        assert_eq!(std::fs::read_to_string(&pidfile).unwrap(), "4242\n");
    }

    // Covers: R11, R164
    /// Truncation is the last thing before `init`: a start that fails before
    /// it, here on a user that does not exist, leaves the previous run's log.
    #[test]
    fn a_start_failing_before_init_leaves_the_previous_log() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "the previous run\n").unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout(&log)
            .user("blivet-no-such-user");

        let result = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::UserNotFound(_))
            ),
            "{result:?}"
        );
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "the previous run\n");
    }

    // Covers: R164
    /// A log emptied for `init` cannot get its content back: a failure from
    /// `init` on says so.
    #[test]
    fn a_log_emptied_before_a_failed_init_is_reported() {
        use crate::ChangedState;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "the previous run\n").unwrap();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).stdout(&log);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "init failed"))
            });

        let err = result.expect_err("init failed the start");
        let report: Vec<_> = err
            .rollback_failures()
            .iter()
            .map(|f| (f.state(), f.path()))
            .collect();
        assert_eq!(report, [(ChangedState::EmptiedOutputFile, Some(&*log))]);
    }

    // Covers: R164
    /// An empty log loses nothing to the truncation.
    #[test]
    fn an_empty_log_is_no_rollback_failure() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        std::fs::write(&log, "").unwrap();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).stdout(&log);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                Err(DaemonizeError::application(75, "init failed"))
            });

        let err = result.expect_err("init failed the start");
        assert!(
            err.rollback_failures().is_empty(),
            "{:?}",
            err.rollback_failures()
        );
    }

    // Covers: R164
    /// Output init left in stdout's buffer, a partial line, reaches the log
    /// before the rollback looks at it, so the log stays.
    #[test]
    fn output_still_buffered_keeps_the_log() {
        use std::io::Write;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).stdout(&log);

        let result: Result<(DaemonContext, ()), _> =
            run_inner_with(&config, &mut NullForker::new(vec![], Ok(())), || {
                write!(std::io::stdout(), "connecting").unwrap();
                Err(DaemonizeError::application(75, "init failed"))
            });

        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&log).ok().as_deref(),
            Some("connecting")
        );
    }

    // Covers: R160, R164
    /// A daemon's process exits on failure, so what its rollback left on disk
    /// reaches the launcher on the pipe, after the error, a line each.
    #[test]
    fn a_daemon_tells_the_launcher_what_its_rollback_left() {
        use std::os::unix::fs::PermissionsExt;
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::both_child_with_reader();
        let read_only = |mode| {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        };

        let exited = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner_with(&config, &mut forker, || -> Result<(), _> {
                read_only(0o555);
                Err(DaemonizeError::application(75, "database unreachable"))
            })
        }));
        read_only(0o755);

        assert!(exited.is_err(), "a daemon-mode failure exits");
        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader");
        match notify::decode(&crate::test_support::read_pipe(rd)) {
            notify::Outcome::Failure { code: 75, message } => {
                let mut lines = message.lines();
                assert_eq!(
                    lines.next(),
                    Some("application error: database unreachable")
                );
                let line = lines.next().unwrap_or_default();
                assert!(
                    line.starts_with(&format!("rollback left pidfile {}: ", pidfile.display())),
                    "{message}"
                );
            }
            _ => panic!("the launcher did not read a failure with code 75"),
        }
    }

    // Covers: R160, R164, R166
    /// A panic in init fails the start as an error does, so the launcher
    /// hears what the rollback left too.
    #[test]
    fn a_panicking_daemon_tells_the_launcher_what_its_rollback_left() {
        use std::os::unix::fs::PermissionsExt;
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::both_child_with_reader();
        let read_only = |mode| {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        };

        let _ = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner_with(&config, &mut forker, || -> Result<(), _> {
                read_only(0o555);
                panic!("database schema too new")
            })
        }));
        read_only(0o755);

        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader");
        match notify::decode(&crate::test_support::read_pipe(rd)) {
            notify::Outcome::Failure { message, .. } => assert!(
                message.contains(&format!("rollback left pidfile {}: ", pidfile.display())),
                "{message}"
            ),
            notify::Outcome::Success => panic!("the launcher read a success"),
        }
    }

    // Covers: R164, R166
    /// A panic flushes what init left in stdout's buffer before the rollback
    /// looks at the log, as an error does.
    #[test]
    fn a_panicking_daemon_keeps_output_still_buffered() {
        use std::io::Write;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let mut config = DaemonConfig::new();
        config.close_fds(false).stdout(&log);
        let mut forker = NullForker::both_child_with_reader();

        let _ = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner_with(&config, &mut forker, || -> Result<(), _> {
                write!(std::io::stdout(), "connecting").unwrap();
                panic!("database schema too new")
            })
        }));

        assert_eq!(
            std::fs::read_to_string(&log).ok().as_deref(),
            Some("connecting")
        );
    }

    // Covers: R164, R170
    /// A daemon whose context drops before it tells the launcher it is
    /// ready never started: the log it created goes, as on an init failure.
    #[test]
    fn a_context_dropped_before_readiness_rolls_the_start_back() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let mut config = DaemonConfig::new();
        config.close_fds(false).stdout(&log);
        let mut forker = NullForker::both_child_with_reader();

        let (ctx, ()) = run_inner_with(&config, &mut forker, || Ok(())).unwrap();
        drop(ctx);

        assert!(!log.exists(), "the log this start created stayed");
    }

    // Covers: R160, R164, R170
    /// The launcher of a daemon whose context dropped before readiness hears
    /// what the rollback could not put back.
    #[test]
    fn a_context_dropped_before_readiness_tells_the_launcher_what_it_left() {
        use std::os::unix::fs::PermissionsExt;
        if !isolated() {
            return;
        }
        crate::test_support::shed_root();
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let mut config = DaemonConfig::new();
        config.close_fds(false).stdout(&log);
        let mut forker = NullForker::both_child_with_reader();
        let read_only = |mode| {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        };

        let (ctx, ()) = run_inner_with(&config, &mut forker, || Ok(())).unwrap();
        read_only(0o555);
        drop(ctx);
        read_only(0o755);

        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader");
        match notify::decode(&crate::test_support::read_pipe(rd)) {
            notify::Outcome::Failure { message, .. } => assert!(
                message.contains(&format!("rollback left output file {}: ", log.display())),
                "{message}"
            ),
            notify::Outcome::Success => panic!("the launcher read a success"),
        }
    }

    // Covers: R164, R170
    /// What the daemon left in stdout's buffer reaches the log before the
    /// rollback looks at it, so a log with output stays.
    #[test]
    fn a_context_dropped_before_readiness_keeps_output_still_buffered() {
        use std::io::Write;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let mut config = DaemonConfig::new();
        config.close_fds(false).stdout(&log);
        let mut forker = NullForker::both_child_with_reader();

        let (ctx, ()) = run_inner_with(&config, &mut forker, || Ok(())).unwrap();
        write!(std::io::stdout(), "connecting").unwrap();
        drop(ctx);

        assert_eq!(
            std::fs::read_to_string(&log).ok().as_deref(),
            Some("connecting")
        );
    }

    // Covers: R164, R170
    /// A daemon that told its launcher it is ready keeps what its start
    /// made: the log stays after the context drops.
    #[test]
    fn a_ready_daemon_keeps_its_log_when_the_context_drops() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let mut config = DaemonConfig::new();
        config.close_fds(false).stdout(&log);
        let mut forker = NullForker::both_child_with_reader();

        let (mut ctx, ()) = run_inner_with(&config, &mut forker, || Ok(())).unwrap();
        ctx.notify_parent().unwrap();
        drop(ctx);

        assert!(log.exists(), "a started daemon's log was removed");
    }

    // Covers: R164, R166
    /// A foreground panic unwinds past the sequence, and the rollback that
    /// runs on the way flushes stdout first, as an error's does: the log
    /// keeps what init wrote, and the text does not reach the caller's
    /// terminal.
    #[test]
    fn a_foreground_panic_keeps_output_still_buffered() {
        use std::io::Write;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).stdout(&log);

        let _ = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner_with(
                &config,
                &mut NullForker::new(vec![], Ok(())),
                || -> Result<(), _> {
                    write!(std::io::stdout(), "connecting").unwrap();
                    panic!("database schema too new")
                },
            )
        }));

        assert_eq!(
            std::fs::read_to_string(&log).ok().as_deref(),
            Some("connecting")
        );
    }

    // Covers: R154
    #[test]
    fn daemon_init_error_reaches_the_launcher() {
        if !isolated() {
            return;
        }
        let mut config = DaemonConfig::new();
        config.close_fds(false);
        let mut forker = NullForker::both_child();

        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner_with(&config, &mut forker, || -> Result<(), _> {
                Err(DaemonizeError::application(75, "database unreachable"))
            })
        }));

        let panic_msg = result
            .expect_err("a daemon-mode init error exits, not returns")
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(panic_msg.contains("NullForker::exit(75)"), "{panic_msg}");
    }

    #[test]
    fn both_forks_child_succeeds() {
        if !isolated() {
            return;
        }
        let mut config = DaemonConfig::new();
        // The fork arms are under test here; step 13 has its own tests.
        config.close_fds(false);
        let mut forker = NullForker::both_child();
        let result = run_inner(&config, &mut forker);
        assert!(result.is_ok());
    }

    // Covers: R137, R150
    //
    // NullForker runs no daemon, so the parent reads its channel after closing
    // its own write end: what a daemon killed before notifying leaves. The
    // parent must *close* that end, not drop it, or NotifyChannel's Drop safety
    // net writes failure bytes the parent then reads as the daemon's. Both
    // exit 1, so the printed message tells them apart.
    #[test]
    fn launcher_reads_its_own_silent_close_as_the_eof_failure() {
        let Some(output) = crate::test_support::isolated_output() else {
            let config = DaemonConfig::new();
            let mut forker = NullForker::first_parent();
            let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_inner(&config, &mut forker)
            }));
            let panic_msg = result
                .expect_err("parent exits after reading the pipe")
                .downcast_ref::<String>()
                .cloned()
                .unwrap();
            assert!(panic_msg.contains("NullForker::exit(1)"), "{panic_msg}");
            return;
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{}",
            crate::test_support::subprocess_report(&output)
        );
        assert!(
            stderr.contains("daemon ended without signaling readiness"),
            "the launcher did not report EOF: {stderr}"
        );
        assert!(
            !stderr.contains("daemon exited without signaling readiness"),
            "the launcher read its own drop bytes: {stderr}"
        );
    }

    // Covers: R137
    #[test]
    fn second_fork_intermediate_closes_pipe_silently() {
        // The intermediate child's write-end copy must also close without
        // writing: the grandchild daemon is the sole writer on the channel.
        let config = DaemonConfig::new();
        let mut forker = NullForker::second_parent();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        let panic_msg = result
            .expect_err("intermediate child exits")
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(panic_msg.contains("NullForker::exit(0)"), "{panic_msg}");

        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader for the test");
        let buf = crate::test_support::read_pipe(rd);
        assert_eq!(
            buf,
            Vec::<u8>::new(),
            "intermediate child must close the pipe without writing; the \
             daemon is the sole writer"
        );
    }

    // Covers: R57
    #[test]
    fn first_fork_failure_closes_pipe_silently() {
        // The error goes back to the caller directly — nothing should reach the
        // channel, whose only reader is this same process. A plain drop would
        // fire the NotifyChannel Drop safety net into it.
        let config = DaemonConfig::new();
        let mut forker = NullForker::first_fork_fails();
        let result = run_inner(&config, &mut forker);
        assert!(matches!(
            result.as_ref().map_err(DaemonizeError::kind),
            Err(ErrorKind::ForkFailed(_))
        ));

        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader");
        let buf = crate::test_support::read_pipe(rd);
        assert_eq!(
            buf,
            Vec::<u8>::new(),
            "a pre-fork failure must leave the pipe unwritten"
        );
    }

    #[test]
    fn setsid_failure_reports_to_the_parent_and_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::setsid_fails();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        let panic_msg = result
            .expect_err("the child exits")
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(panic_msg.contains("NullForker::exit(71)"), "{panic_msg}");

        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader");
        let buf = crate::test_support::read_pipe(rd);
        assert_eq!(
            buf,
            notify::error_bytes(&ErrorKind::SetsidFailed("test".into()).into())
        );
    }

    // Covers: R58
    #[test]
    fn second_fork_failure_reports_to_the_parent_and_exits() {
        let config = DaemonConfig::new();
        let mut forker = NullForker::second_fork_fails();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        let panic_msg = result
            .expect_err("the child exits")
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(panic_msg.contains("NullForker::exit(71)"), "{panic_msg}");

        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader");
        let buf = crate::test_support::read_pipe(rd);
        assert_eq!(
            buf,
            notify::error_bytes(&ErrorKind::ForkFailed("second fork".into()).into())
        );
    }

    // A post-fork step failing in daemon mode must funnel through the
    // notify-parent-and-exit arm in daemonize_inner_with, not return the error to the caller —
    // the child has no caller to return to. Distinct from a fork failure, which
    // returns before run_post_fork. Kills the mutant that swaps the foreground
    // and daemon match arms: with the arms swapped, this would return Err
    // instead of exiting.
    #[test]
    fn daemon_post_fork_failure_exits_not_returns() {
        if !isolated() {
            return;
        }
        // Daemon mode (foreground stays false); chdir to a nonexistent path
        // fails at step 5, before any stdio redirection touches this process.
        let mut config = DaemonConfig::new();
        config.chdir("/nonexistent_daemonize_postfork_failure");
        let mut forker = NullForker::both_child();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));

        let panic_msg = result
            .expect_err("daemon-mode post-fork failure must exit (panic), not return")
            .downcast_ref::<String>()
            .cloned()
            .unwrap();
        assert!(
            panic_msg.contains("NullForker::exit(71)"),
            "must exit with ChdirFailed's EX_OSERR code 71, got: {panic_msg}"
        );
    }

    // Covers: R166
    /// In the daemon, unwinding out of the sequence would run the caller's
    /// frames, and any `catch_unwind` recovery in them, in a detached process
    /// whose launcher has already reported failure. The panic must end the
    /// daemon like an error does, with its text on the channel.
    #[test]
    fn daemon_init_panic_exits_and_reports_its_text() {
        if !isolated() {
            return;
        }
        let mut config = DaemonConfig::new();
        config.close_fds(false);
        let mut forker = NullForker::both_child_with_reader();
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner_with(&config, &mut forker, || -> Result<(), _> {
                panic!("database schema too new")
            })
        }));

        let panic_msg = result
            .expect_err("a daemon must exit after a panic, not return")
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(
            panic_msg.contains("NullForker::exit(70)"),
            "the daemon unwound instead of exiting: {panic_msg}"
        );
        let rd = forker
            .take_pipe_reader()
            .expect("NullForker stores a reader");
        match notify::decode(&crate::test_support::read_pipe(rd)) {
            notify::Outcome::Failure { code: 70, message } => assert!(
                message.contains("database schema too new"),
                "the panic text was lost: {message}"
            ),
            _ => panic!("the launcher did not read a failure with code 70"),
        }
    }

    // Covers: R66, R68, R121
    #[test]
    fn foreground_mode_skips_fork() {
        use nix::sys::stat::fstat;

        if !isolated() {
            return;
        }
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false);
        // No fork is scripted, so a fork panics; setsid is scripted to fail,
        // so a setsid surfaces as an error. Either would fail the run.
        let mut forker = NullForker::new(
            vec![],
            Err(ErrorKind::SetsidFailed("foreground must not setsid".into())),
        );
        let stdout_before = fstat(std::io::stdout()).unwrap();

        let result = run_inner(&config, &mut forker);
        let ctx = result.expect("foreground daemonize_inner should succeed");
        assert!(ctx.lockfile_fd().is_none());

        // Step 6 leaves stdout inherited in foreground mode, rather than
        // pointing it at /dev/null.
        let stdout_after = fstat(std::io::stdout()).unwrap();
        assert_eq!(
            (stdout_before.st_dev, stdout_before.st_ino),
            (stdout_after.st_dev, stdout_after.st_ino),
            "foreground mode redirected stdout"
        );
    }

    // Covers: R148
    #[test]
    fn notification_pipe_failure_returns_err_before_any_fork() {
        let config = DaemonConfig::new();
        // No fork is scripted, so a fork panics.
        let mut forker = NullForker::new(vec![], Ok(())).with_failing_pipe();

        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));

        let result = result.expect("a pipe failure must return, not fork or exit");
        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::SystemError(_))
            ),
            "{result:?}"
        );
    }

    // Covers: R164
    /// A failure after both slots moved, here step 15's truncation, puts fd 1
    /// and fd 2 back as they were, and the logs keep the previous run.
    #[test]
    fn a_failed_truncation_puts_both_slots_back() {
        use nix::sys::stat::fstat;
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("app.log");
        let err = dir.path().join("app.err");
        std::fs::write(&out, "previous out\n").unwrap();
        std::fs::write(&err, "previous err\n").unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout(&out)
            .stderr(&err);
        let id = |st: nix::sys::stat::FileStat| (st.st_dev, st.st_ino);
        let before = (
            id(fstat(std::io::stdout()).unwrap()),
            id(fstat(std::io::stderr()).unwrap()),
        );
        steps::failpoints::STREAM_TRUNCATE_FAILS.set();

        let result = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        let after = (
            id(fstat(std::io::stdout()).unwrap()),
            id(fstat(std::io::stderr()).unwrap()),
        );
        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::OutputFileError(_))
            ),
            "{result:?}"
        );
        assert_eq!(before, after, "a failed start left fd 1 or fd 2 moved");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "previous out\n");
        assert_eq!(std::fs::read_to_string(&err).unwrap(), "previous err\n");
    }

    // Covers: R164
    /// The fd 2 counterpart with fd 2 closed: a later failure closes it again.
    #[test]
    fn a_failed_truncation_leaves_a_closed_stderr_closed() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let err = dir.path().join("app.err");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).stderr(&err);
        nix::unistd::close(2).unwrap();
        steps::failpoints::STREAM_TRUNCATE_FAILS.set();

        let result = run_inner(&config, &mut NullForker::new(vec![], Ok(())));

        assert!(result.is_err());
        assert_eq!(
            nix::fcntl::fcntl(std::io::stderr(), nix::fcntl::FcntlArg::F_GETFD),
            Err(nix::errno::Errno::EBADF),
            "a failed start left fd 2 open"
        );
    }

    // Covers: R147
    /// A foreground caller sees its stdio after a failed start, so step 12
    /// must save and restore fd 1 there.
    #[test]
    fn foreground_redirect_that_fails_puts_stdout_back() {
        use nix::sys::stat::fstat;

        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut config = DaemonConfig::new();
        config
            .foreground(true)
            .close_fds(false)
            .stdout(dir.path().join("app.log"));
        let mut forker = NullForker::new(vec![], Ok(()));
        let before = fstat(std::io::stdout()).unwrap();
        steps::failpoints::STREAM_TRUNCATE_FAILS.set();

        let result = run_inner(&config, &mut forker);

        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::OutputFileError(_))
            ),
            "{result:?}"
        );
        let after = fstat(std::io::stdout()).unwrap();
        assert_eq!(
            (before.st_dev, before.st_ino),
            (after.st_dev, after.st_ino),
            "a failed foreground step 12 left fd 1 moved"
        );
    }

    // Covers: R147
    /// Step 12 saves fd 1 only where a failure could be seen: a daemon child
    /// that inherited descriptors up to its limit still gets past it.
    #[test]
    fn daemon_redirect_starts_with_no_descriptor_free_to_save_stdout() {
        use nix::sys::stat::{fstat, stat};

        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("app.log");
        let mut config = DaemonConfig::new();
        config.close_fds(false).stdout(&log);
        let mut forker = NullForker::both_child();

        // Fill every descriptor under a small limit, then free two, which the
        // notification channel's ends take. The child drops its read end, freeing
        // one; step 6 borrows it for /dev/null and gives it back, and the
        // stdout file then takes it, so saving fd 1 would find none.
        let held = crate::test_support::fill_fd_table(2);

        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_inner(&config, &mut forker)
        }));
        let mut ctx = result
            .expect("a daemon with no free descriptor must start, not exit")
            .unwrap();
        // No descriptor is free: the context holds the stdout file's own, to
        // undo a failed start. One free would have let step 12 save fd 1, and
        // the test would prove nothing.
        let spare = std::os::fd::AsFd::as_fd(&std::io::stdin()).try_clone_to_owned();
        drop(held);
        let _ = ctx.notify_parent();
        drop(ctx);
        assert!(spare.is_err(), "a descriptor was free: {spare:?}");
        let on_fd1 = fstat(std::io::stdout()).unwrap();
        let file = stat(&log).unwrap();
        assert_eq!(
            (on_fd1.st_dev, on_fd1.st_ino),
            (file.st_dev, file.st_ino),
            "fd 1 is not the stdout file"
        );
    }

    // The three SystemError-producing steps cannot be made to fail from inside
    // a test process (that needs a missing /dev/null or a seccomp filter), so
    // each propagation test flips a steps::failpoints flag and asserts the
    // error surfaces from daemonize_inner instead of being swallowed — killing
    // mutants like `let _ = steps::clear_signal_mask();` in run_post_fork.
    // Subprocess-isolated: the flags are process-global, and run_post_fork
    // mutates umask/cwd/stdin for real on the way to the failing step.

    // Covers: R95
    #[test]
    fn devnull_failure_propagates() {
        if !isolated() {
            return;
        }
        steps::failpoints::DEVNULL_OPEN_FAILS.set();
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false);
        let mut forker = NullForker::new(vec![], Ok(()));
        match run_inner(&config, &mut forker)
            .as_ref()
            .map_err(DaemonizeError::kind)
        {
            Err(ErrorKind::SystemError(msg)) => {
                assert!(
                    msg.contains("/dev/null"),
                    "message names the syscall: {msg}"
                );
            }
            other => panic!("expected SystemError to propagate, got {other:?}"),
        }
    }

    // Covers: R145
    #[test]
    fn pidfile_write_failure_leaves_no_pidfile() {
        if !isolated() {
            return;
        }
        steps::failpoints::PIDFILE_WRITE_FAILS.set();
        let dir = crate::test_support::tmp_dir();
        let pidfile = dir.join(format!("pidwrite-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pidfile);
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        match run_inner(&config, &mut forker)
            .as_ref()
            .map_err(DaemonizeError::kind)
        {
            Err(ErrorKind::PidfileError(msg)) => {
                assert!(msg.contains("write"), "message names the operation: {msg}");
            }
            other => panic!("expected PidfileError to propagate, got {other:?}"),
        }
        assert!(
            !pidfile.exists(),
            "step 8 failed after creating the pidfile and left it behind, \
             empty, naming no process at all"
        );
    }

    // Covers: R145
    #[test]
    fn pidfile_open_failure_leaves_the_existing_file_alone() {
        if !isolated() {
            return;
        }
        steps::failpoints::PIDFILE_OPEN_FAILS.set();
        let dir = crate::test_support::tmp_dir();
        let pidfile = dir.join(format!("pidopen-{}.pid", std::process::id()));
        std::fs::write(&pidfile, "4242\n").unwrap();
        let mut config = DaemonConfig::new();
        // No lockfile, so step 7 does not open the pidfile and step 8's own
        // open is the first time this process would touch it.
        config
            .foreground(true)
            .close_fds(false)
            .pidfile(&pidfile)
            .no_lockfile();
        let mut forker = NullForker::new(vec![], Ok(()));
        let result = run_inner(&config, &mut forker);
        let left = std::fs::read_to_string(&pidfile);
        let _ = std::fs::remove_file(&pidfile);

        assert!(
            matches!(result.as_ref().map_err(DaemonizeError::kind), Err(ErrorKind::PidfileError(m)) if m.contains("open")),
            "expected the open failure to propagate, got {result:?}"
        );
        assert_eq!(
            left.ok().as_deref(),
            Some("4242\n"),
            "an open that failed removed a pidfile this process never opened"
        );
    }

    // Covers: R134, R145
    #[test]
    fn sigaction_failure_propagates() {
        if !isolated() {
            return;
        }
        steps::failpoints::SIGACTION_FAILS.set();
        let dir = crate::test_support::tmp_dir();
        let pidfile = dir.join(format!("sigaction-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pidfile);
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        match run_inner(&config, &mut forker)
            .as_ref()
            .map_err(DaemonizeError::kind)
        {
            Err(ErrorKind::SystemError(msg)) => {
                assert!(
                    msg.contains("sigaction"),
                    "message names the syscall: {msg}"
                );
            }
            other => panic!("expected SystemError to propagate, got {other:?}"),
        }
        assert!(
            !pidfile.exists(),
            "a sequence that aborted after step 8 left its pidfile behind, \
             naming a process that never started"
        );
    }

    // Covers: R134, R145
    #[test]
    fn sigprocmask_failure_propagates() {
        if !isolated() {
            return;
        }
        steps::failpoints::SIGPROCMASK_FAILS.set();
        let dir = crate::test_support::tmp_dir();
        let pidfile = dir.join(format!("sigprocmask-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pidfile);
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        match run_inner(&config, &mut forker)
            .as_ref()
            .map_err(DaemonizeError::kind)
        {
            Err(ErrorKind::SystemError(msg)) => {
                assert!(
                    msg.contains("sigprocmask"),
                    "message names the syscall: {msg}"
                );
            }
            other => panic!("expected SystemError to propagate, got {other:?}"),
        }
        assert!(
            !pidfile.exists(),
            "a sequence that aborted after step 8 left its pidfile behind, \
             naming a process that never started"
        );
    }

    // Covers: R105, R145
    #[test]
    fn getrlimit_failure_propagates() {
        if !isolated() {
            return;
        }
        // Force the brute-force fallback (the fd listing normally short-circuits
        // it on Linux/macOS), then fail its getrlimit. get_max_fd errors before
        // any fd is closed, so the subprocess's descriptors survive.
        steps::failpoints::FD_LISTING_UNAVAILABLE.set();
        steps::failpoints::GETRLIMIT_FAILS.set();
        let dir = crate::test_support::tmp_dir();
        let pidfile = dir.join(format!("getrlimit-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pidfile);
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(true).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        match run_inner(&config, &mut forker)
            .as_ref()
            .map_err(DaemonizeError::kind)
        {
            Err(ErrorKind::SystemError(msg)) => {
                assert!(
                    msg.contains("getrlimit"),
                    "message names the syscall: {msg}"
                );
            }
            other => panic!("expected SystemError to propagate, got {other:?}"),
        }
        assert!(
            !pidfile.exists(),
            "a sequence that aborted after step 8 left its pidfile behind, \
             naming a process that never started"
        );
    }

    // Covers: R131
    #[test]
    fn pidfile_only_holds_derived_lock() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        let ctx = run_inner(&config, &mut forker).expect("daemonize should succeed");
        let lock_fd = ctx
            .lockfile_fd()
            .expect("a lone pidfile should be flock'd by default");
        // The held lock must be on the pidfile itself, not some other file.
        let lock_stat = nix::sys::stat::fstat(lock_fd).unwrap();
        let pidfile_stat = nix::sys::stat::stat(&pidfile).unwrap();
        assert_eq!(
            (lock_stat.st_dev, lock_stat.st_ino),
            (pidfile_stat.st_dev, pidfile_stat.st_ino),
            "derived lock fd should refer to the pidfile"
        );
        // A second acquisition of the same path must conflict.
        let second = steps::open_and_lock(&pidfile);
        assert!(matches!(second, Err(ErrorKind::LockConflict { .. })));
    }

    // Covers: R134, R145
    #[test]
    fn foreground_lock_conflict_returns_err() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("app.pid");
        let _held = steps::open_and_lock(&pidfile).expect("first lock should succeed");
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false).pidfile(&pidfile);
        let mut forker = NullForker::new(vec![], Ok(()));
        // The running daemon's own pidfile, which this attempt must not touch.
        std::fs::write(&pidfile, "999999\n").expect("the incumbent's pidfile");

        let result = run_inner(&config, &mut forker);
        assert!(
            matches!(
                result.as_ref().map_err(DaemonizeError::kind),
                Err(ErrorKind::LockConflict { .. })
            ),
            "foreground mode should surface setup errors as Err, not exit"
        );
        // The abort guard removes the pidfile a failed sequence wrote. This
        // sequence failed at step 7, before step 8 wrote anything, so the file
        // on disk belongs to the daemon already running — deleting it would
        // strand whoever is supervising it.
        assert_eq!(
            std::fs::read_to_string(&pidfile).expect("the incumbent's pidfile survives"),
            "999999\n",
            "a lock conflict removed the running daemon's pidfile"
        );
    }

    // Covers: R67
    #[test]
    fn foreground_mode_notify_parent_noop() {
        if !isolated() {
            return;
        }
        let mut config = DaemonConfig::new();
        config.foreground(true).close_fds(false);
        let mut forker = NullForker::new(vec![], Ok(()));
        let mut ctx = run_inner(&config, &mut forker).unwrap();
        assert!(ctx.notify_parent().is_ok());
    }

    // Covers: R69, R122
    #[test]
    #[allow(clippy::disallowed_methods)] // a test pipe or file, not the channel: SIGPIPE cannot hit it
    fn close_fds_false_preserves_fds() {
        if !isolated() {
            return;
        }
        let (rd, wr) = crate::test_support::make_pipe();

        let mut config = DaemonConfig::new();
        config.close_fds(false);
        let mut forker = NullForker::both_child();
        let _ctx = run_inner(&config, &mut forker).unwrap();

        assert!(
            nix::unistd::write(&wr, b"alive").is_ok(),
            "write fd should still be open with close_fds=false"
        );
        let mut buf = [0u8; 5];
        assert!(
            nix::unistd::read(&rd, &mut buf).is_ok(),
            "read fd should still be open with close_fds=false"
        );
    }

    // Covers: R120
    #[test]
    fn context_carries_config_fields() {
        if !isolated() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("test.pid");
        let stdout = dir.path().join("out.log");

        let mut config = DaemonConfig::new();
        config
            .pidfile(&pidfile)
            .stdout(&stdout)
            .foreground(true)
            .close_fds(false);

        let mut forker = NullForker::new(vec![], Ok(()));
        let ctx = run_inner(&config, &mut forker).unwrap();

        let debug = format!("{:?}", ctx);
        assert!(
            debug.contains("test.pid"),
            "context should contain pidfile path"
        );
        assert!(
            debug.contains("out.log"),
            "context should contain stdout path"
        );
        // User and group: context::tests::context_stores_config_fields. A
        // sequence with them set would switch identity, which needs root.
    }
}
