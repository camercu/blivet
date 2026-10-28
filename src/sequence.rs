//! The daemonize sequence: the forks, the post-fork steps in order, the
//! guard that undoes them when the sequence aborts, and the
//! launcher's side of the notification channel.
//!
//! The entry points in `lib.rs` validate and check the thread count, then
//! call [`daemonize_inner_with`]; the individual steps live in `steps/`.

use std::os::fd::{AsRawFd, OwnedFd};

use nix::fcntl::Flock;
use nix::unistd::ForkResult;

use crate::forker::Forker;
use crate::identity::ResolvedIdentity;
use crate::notify::{self, NotifyChannel};
use crate::{DaemonConfig, DaemonContext, DaemonizeError, signals, steps};

/// Internal daemonization logic, generic over the Forker trait for testability.
///
/// # Safety
///
/// With a real `Forker` this performs `fork`; the process must be
/// single-threaded at the call, since forking with other threads running is
/// undefined behavior. The checked [`daemonize`] establishes this, and the
/// public [`daemonize_unchecked`] forwards the contract to its caller. (The
/// test `NullForker` does not fork, so test calls are sound.)
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
                return Err(e);
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
    let run = || unsafe { run_post_fork(config, &mut pipe_wr, init) };
    let result = if foreground {
        run()
    } else {
        // A panic (in practice, init's) ends the daemon as an error does:
        // unwinding on would run the caller's frames in a detached process
        // whose launcher has already read a failure (R166).
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
            .unwrap_or_else(|panic| Err(panicked(&*panic)))
    };
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
fn panicked(payload: &(dyn std::any::Any + Send)) -> DaemonizeError {
    let text = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message");
    DaemonizeError::application(70, format!("daemon panicked during startup: {text}"))
}

/// What a failed sequence undoes (R164): each step's [`Undo`](steps::Undo)
/// token, in the reverse of the order the steps ran, then the pidfile (step 8),
/// and last the lock (step 7). A half-done identity switch (step 17) undoes
/// itself.
///
/// In foreground mode the process is the caller's, and it gets back the parts
/// that can be restored. In daemon mode the process exits on failure, so the
/// tokens hold nothing and only the pidfile is removed. What cannot be
/// restored is in R164. [`disarm`](Self::disarm) at step 18 keeps every change
/// and hands over the lock.
///
/// The pidfile goes while the lock is still held: released first, the lock
/// would let a second instance in to write its pid into a file this one then
/// removes. Fields drop after the body of [`Drop::drop`], so the lock in
/// `files` is released after the body has removed the pidfile.
struct AbortGuard<'a> {
    /// `PutBack` in foreground mode, `Skip` in a daemon: the one place the
    /// sequence decides how much a failure undoes.
    rollback: steps::Rollback,
    /// Each step's token, in the order the steps ran.
    undo: Vec<steps::Undo>,
    /// The checkpoints passed, where the undo walk may fail the sequence.
    #[cfg(test)]
    checkpoints: usize,
    /// The pidfile this process owns from the second half of step 8: written
    /// or truncated by it, so a failure must not leave it naming a process
    /// that never started. Removed whatever
    /// [`cleanup_on_drop`](crate::DaemonConfig::cleanup_on_drop) says, which
    /// governs only a daemon that started (R145).
    pidfile: Option<&'a std::path::Path>,
    /// The files steps 7, 8 and 12 opened, the lock among them.
    files: steps::OpenedFiles<'a>,
}

impl AbortGuard<'_> {
    fn new(foreground: bool) -> Self {
        Self {
            rollback: if foreground {
                steps::Rollback::PutBack
            } else {
                steps::Rollback::Skip
            },
            undo: Vec::new(),
            #[cfg(test)]
            checkpoints: 0,
            pidfile: None,
            files: steps::OpenedFiles::default(),
        }
    }

    /// Whether steps save what they change.
    fn rollback(&self) -> steps::Rollback {
        self.rollback
    }

    /// Holds a step's token, to drop on failure.
    fn record(&mut self, undo: steps::Undo) -> Result<(), DaemonizeError> {
        self.undo.push(undo);
        self.checkpoint()
    }

    /// A point the undo walk may fail the sequence at; nothing outside tests.
    fn checkpoint(&mut self) -> Result<(), DaemonizeError> {
        #[cfg(test)]
        {
            self.checkpoints += 1;
            if steps::failpoints::checkpoint_fails(self.checkpoints) {
                return Err(DaemonizeError::SystemError(
                    steps::failpoints::CHECKPOINT_FAILURE.into(),
                ));
            }
        }
        Ok(())
    }

    /// As [`record`](Self::record), for each of a step's tokens.
    fn record_all(&mut self, undo: Vec<steps::Undo>) -> Result<(), DaemonizeError> {
        let mut undo = undo.into_iter();
        while let Some(next) = undo.next() {
            if let Err(e) = self.record(next) {
                // The rest restore now, latest first, before the recorded.
                undo.rev().for_each(drop);
                return Err(e);
            }
        }
        Ok(())
    }

    /// From `init` on other threads may run, so a token whose restore is
    /// unsafe then (the env's `setenv`) restores nothing (R164).
    fn threads_may_start(&mut self) {
        self.undo
            .iter_mut()
            .for_each(steps::Undo::threads_may_start);
    }

    /// The descriptors the tokens hold, which the fd sweep must spare.
    fn held_fds(&self) -> impl Iterator<Item = i32> + '_ {
        self.undo.iter().filter_map(steps::Undo::held_fd)
    }

    /// The sequence finished: keep every change, and hand over the lock.
    fn disarm(&mut self) -> Option<Flock<OwnedFd>> {
        self.undo.drain(..).for_each(steps::Undo::keep);
        self.pidfile = None;
        self.files.take_lock()
    }
}

impl Drop for AbortGuard<'_> {
    fn drop(&mut self) {
        // Latest first: each token restores as it drops.
        while self.undo.pop().is_some() {}
        if let Some(path) = self.pidfile.take() {
            let _ = std::fs::remove_file(path);
        }
        // `files` drops after this body, so the pidfile is gone first.
    }
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
    // mode, where the process is the caller's; only the pidfile in a daemon,
    // whose process exits on failure.
    let mut abort = AbortGuard::new(config.foreground);

    // Step 4: Set umask
    abort.record(steps::set_umask(config.umask, abort.rollback()))?;

    // Step 5: chdir
    abort.record(steps::change_dir(&config.chdir, abort.rollback())?)?;

    // Step 6: Redirect stdin to /dev/null (always); redirect stdout/stderr
    // to /dev/null only when not in foreground mode (foreground leaves them
    // inherited so output reaches the terminal or supervisor).
    abort.record_all(steps::redirect_to_devnull(
        !config.foreground,
        abort.rollback(),
    )?)?;

    // Step 7: Open and lock lockfile (explicit path, or the pidfile itself
    // unless derivation was opted out)
    let lockfile_path = config.effective_lockfile().map(std::path::PathBuf::as_path);
    if let Some(path) = lockfile_path {
        let lock = steps::open_and_lock(path)?;
        let id = crate::file_identity::file_id(&*lock)
            .map_err(|e| DaemonizeError::LockfileError(format!("fstat: {e}")))?;
        abort.files.push(path, steps::Role::Lockfile(lock), id);
    }

    // Step 8: Write pidfile. The debt is owed from the moment this process
    // owns the file on disk — once `open_pidfile` returns, having locked it at
    // step 7 or created and truncated it just now — not from the moment the
    // write succeeds: a write that fails half way leaves an empty file naming
    // no process at all. Nor any earlier: an `open_pidfile` that fails, or a
    // lock conflict at step 7, leaves a file that belongs to someone else.
    // The abort guard removes the pidfile from the second half of step 8 on.
    let mut pidfile_id = None;
    if let Some(ref pidfile_path) = config.pidfile {
        let pidfile = steps::open_pidfile(pidfile_path, abort.files.lock())?;
        abort.pidfile = Some(pidfile_path);
        let (id, fd) = pidfile.write_pid()?;
        pidfile_id = Some(id);
        // A standalone pidfile's descriptor, kept for step 16's chown (R163).
        if let Some(fd) = fd {
            abort.files.push(pidfile_path, steps::Role::Pidfile(fd), id);
        }
    }

    // Step 9: Reset signal dispositions
    abort.record(signals::reset_signal_dispositions(abort.rollback())?)?;

    // Step 10: Clear signal mask
    abort.record(steps::clear_signal_mask(abort.rollback())?)?;

    // Step 11: Set environment variables
    abort.record(steps::set_env_vars(&config.env, abort.rollback()))?;

    // Step 12: Redirect stdout/stderr to configured files. Only a foreground
    // caller sees the slots after a failure, so only it pays for saving them.
    // The files it opened stay held for step 16's chown (R163).
    if config.stdout.is_some() || config.stderr.is_some() {
        let redirected = steps::redirect_output(
            config.stdout.as_deref(),
            config.stderr.as_deref(),
            config.append,
            &abort.files.owned(pidfile_id),
            abort.rollback(),
        )?;
        abort.record_all(redirected.slots)?;
        for (path, fd, id) in redirected.files {
            abort.files.push(path, steps::Role::Stream(fd), id);
        }
    }

    // Step 13: Close inherited fds (if enabled)
    if config.close_fds {
        let pipe = pipe_wr.as_ref().map(|wr| wr.as_fd().as_raw_fd());
        let spared = steps::spared_fds(&abort.files, abort.held_fds(), pipe);
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
        abort.record(identity.join_groups(abort.rollback())?)?;
    }

    // Step 15: The caller's privileged init (R153). After the lock, so only
    // the instance holding it runs; after the fd sweep, so what it opens
    // stays open. An error is the sequence's, and the abort guard removes the
    // pidfile (R154). From here other threads may run, and `setenv` with one
    // alive is undefined behaviour, so a failure leaves the env as step 11
    // set it.
    // A change between the last token and here, not undone, fails the walk.
    abort.checkpoint()?;
    abort.threads_may_start();
    let value = init()?;

    // Steps 16-17: Hand the daemon's files to the target identity while still
    // privileged, then switch to it (R155).
    if let Some(identity) = &identity {
        // The files as opened, by descriptor (R163); for stdout and stderr,
        // step 12's, as init may have moved fds 1 and 2.
        identity.chown_files(steps::chown_targets(&abort.files, config.chown_paths))?;
    }
    #[cfg(test)]
    if steps::failpoints::BEFORE_SWITCH_FAILS.injected() {
        return Err(DaemonizeError::SystemError(
            steps::failpoints::SWITCH_FAILURE.into(),
        ));
    }
    if let Some(identity) = identity {
        // SAFETY: this function's contract requires a single thread, and the
        // checked entry points verify it again after init (R126).
        unsafe { identity.switch_to() }?;
    }

    // Step 18: Return DaemonContext (clones the config-derived fields it needs).
    // The context owns the pidfile and the lock from here, so the abort guard
    // stands down.
    let lockfile = abort.disarm();
    Ok((DaemonContext::new(config, lockfile, pipe_wr.take()), value))
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
            matches!(result, Err(DaemonizeError::LockConflict { .. })),
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
            matches!(result, Err(DaemonizeError::OutputFileError(_))),
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
            matches!(&result, Err(DaemonizeError::ChownError(m)) if m.contains("app.pid")),
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
            matches!(&result, Err(DaemonizeError::PermissionDenied(m)) if m.contains("setgid")),
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
            matches!(&result, Err(DaemonizeError::PermissionDenied(m)) if m.contains("setgid")),
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
            matches!(&result, Err(DaemonizeError::PermissionDenied(m)) if m.contains("setgid")),
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
            matches!(result, Err(DaemonizeError::UserNotFound(_))),
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
            matches!(&result, Err(DaemonizeError::ChownError(m)) if m.contains("app.pid")),
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
            matches!(&result, Err(DaemonizeError::PermissionDenied(m)) if m.contains("initgroups")),
            "{result:?}"
        );
        assert!(!ran, "init ran before the supplementary groups were set");
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
            matches!(&result, Err(DaemonizeError::PermissionDenied(m)) if m.contains("setuid")),
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
        let umask_before = umask(Mode::from_bits_truncate(0o022));
        umask(umask_before);

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
        assert!(matches!(result, Err(DaemonizeError::ForkFailed(_))));

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
            notify::error_bytes(&DaemonizeError::SetsidFailed("test".into()))
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
            notify::error_bytes(&DaemonizeError::ForkFailed("second fork".into()))
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
            Err(DaemonizeError::SetsidFailed(
                "foreground must not setsid".into(),
            )),
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
            matches!(result, Err(DaemonizeError::SystemError(_))),
            "{result:?}"
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
            matches!(result, Err(DaemonizeError::OutputFileError(_))),
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
        // Exactly one descriptor is free now: the stdout file's own, closed
        // once it was on fd 1. A second would have let step 12 save fd 1, and
        // the test would prove nothing.
        let clone = || std::os::fd::AsFd::as_fd(&std::io::stdin()).try_clone_to_owned();
        let spare = (clone(), clone());
        drop(held);

        let ctx = result
            .expect("a daemon with no free descriptor must start, not exit")
            .unwrap();
        drop(ctx);
        assert!(
            spare.0.is_ok() && spare.1.is_err(),
            "not exactly one descriptor free: {spare:?}"
        );
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
        match run_inner(&config, &mut forker) {
            Err(DaemonizeError::SystemError(msg)) => {
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
        match run_inner(&config, &mut forker) {
            Err(DaemonizeError::PidfileError(msg)) => {
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
            matches!(&result, Err(DaemonizeError::PidfileError(m)) if m.contains("open")),
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
        match run_inner(&config, &mut forker) {
            Err(DaemonizeError::SystemError(msg)) => {
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
        match run_inner(&config, &mut forker) {
            Err(DaemonizeError::SystemError(msg)) => {
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
        match run_inner(&config, &mut forker) {
            Err(DaemonizeError::SystemError(msg)) => {
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
        assert!(matches!(second, Err(DaemonizeError::LockConflict { .. })));
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
            matches!(result, Err(DaemonizeError::LockConflict { .. })),
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
