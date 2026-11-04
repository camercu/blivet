# blivet

Turns a Unix process into a daemon, correctly, and makes the process that
started it wait for the daemon to report readiness or a failure with a
`sysexits.h` exit code. Ships as a Rust library and as the `daemonize` CLI,
which wraps any program.

Each term: one line, then what to avoid. The API it names is in the
rustdoc; the steps, exit codes and flags are in `docs/SPEC.md` and
daemonize(1).

## Language

### Processes

**Launcher**:
The process that called `daemonize()` or ran the CLI, waiting for the start's
outcome. _Avoid_: parent (it is the daemon's grandparent), original process,
caller

**Daemon**:
The grandchild that runs the daemonization sequence, then the application.
_Avoid_: child, grandchild (outside step-level detail), background process

**Intermediate child**:
The process between the two forks; it creates the session and exits 0.
_Avoid_: first child, session leader

**Supervisor**:
A service manager (systemd, launchd, a container runtime) that runs the program
in foreground mode. _Avoid_: init system, service manager, process supervisor

### Configuration and setup

**Daemon config**:
The validated choices that drive one daemonization (`DaemonConfig`).
_Avoid_: options, settings

**Daemonization sequence**:
The fixed, ordered steps from launcher to daemon (SPEC steps 1–18).
_Avoid_: daemonization process, setup

**Daemon mode**:
Running the full sequence, with both forks and `setsid`.
_Avoid_: background mode

**Foreground mode**:
Running the sequence without forks, `setsid` or notification channel.
_Avoid_: no-daemon mode, debug mode

**Pidfile**:
A file holding the daemon's process ID.
_Avoid_: PID file, pid file

**Lockfile**:
A file the daemon holds an exclusive `flock` on for life; the pidfile by
default. _Avoid_: lock file, lock (for the file)

**Single-instance enforcement**:
The guarantee, through the lockfile, that one daemon runs per lockfile.
_Avoid_: instance locking

**Lock conflict**:
Another process already holds the lockfile.
_Avoid_: already running

**Privileged init**:
Caller work that runs in the sequence after the lock and before the identity
switch (`daemonize_with`'s `init`). _Avoid_: init (bare), init closure, setup
hook

**Target identity**:
The user and group the daemon runs as after the identity switch.
_Avoid_: target user (when a group is meant too)

**Identity switch**:
Setting the target group and, with a user, the user and its environment
(R162). _Avoid_: user switch, privilege drop, drop privileges

**Path ownership transfer**:
Chowning the pidfile, lockfile and output files to the target identity before
the switch. _Avoid_: chown paths (outside the API)

**Output redirect**:
Pointing stdout or stderr at a configured file instead of `/dev/null`.
_Avoid_: log file, output file (for the act)

**Single-threaded contract**:
No other thread exists across the daemonization sequence.
_Avoid_: single-threaded window, thread check

**Checked entry point**:
`daemonize` or `daemonize_with`: verifies the single-threaded contract.
_Avoid_: safe form

**Unchecked entry point**:
`daemonize_unchecked` or `daemonize_with_unchecked`: leaves the contract to
the caller. _Avoid_: unsafe form

### Start outcome

**Notification channel**:
The one-way socket pair the daemon reports its start over.
_Avoid_: notification pipe (it is not a `pipe(2)`), pipe

**Readiness**:
The daemon's report that it started; the launcher exits 0.
_Avoid_: ready signal, signal readiness (clashes with Unix signals)

**Exec marker**:
The message that the daemon is about to `exec`; a close after it means
success. _Avoid_: exec pending

**Failure report**:
An exit code and text; the launcher prints the text and exits with the code.
_Avoid_: error signal

**Unnotified exit**:
The daemon ending before readiness without a failure report; the launcher
exits 1. _Avoid_: silent death

**Exit code**:
The `sysexits.h` status a failure maps to, or an application error's own code.
_Avoid_: exit status (for the mapped value), return code

**Application error**:
A failure the caller reports with its own exit code (`ErrorKind::Application`).
_Avoid_: custom error, user error

**Rollback failure**:
A change a failed start could not put back, reported beside the error
(`RestoreFailure`). _Avoid_: restore error, cleanup error

**Program handoff**:
Replacing the daemon with another program by `execvp`, keeping the lock.
_Avoid_: spawn, run program

### Shutdown

**Pidfile cleanup**:
Removing the pidfile when the daemon ends; never a standalone lockfile.
_Avoid_: cleanup (bare), teardown

**Stale pidfile**:
A pidfile left on disk after its daemon ended.
_Avoid_: leftover pidfile, orphan pidfile

**Termination signal**:
`SIGINT` or `SIGTERM`, the signals that normally stop a daemon.
_Avoid_: kill signal, term signal

## Relationships

- A **Launcher** waits on exactly one **Daemon** through one **Notification
  channel**.
- A **Daemon** sends at most one of: **Readiness**, **Exec marker** then
  success, or **Failure report**; sending none is an **Unnotified exit**.
- With no explicit **Lockfile**, the **Pidfile** is the **Lockfile**.
- **Privileged init** runs at most once per **Daemon**, after the **Lockfile**
  is locked and before the **Identity switch**.
- **Path ownership transfer** precedes the **Identity switch**.
- **Foreground mode** has no **Launcher** and no **Notification channel**;
  failures return to the caller.

## Open naming questions

- "parent" in `notify_parent` means the **Launcher**. Consider `notify_ready`
  at the next breaking release.
- `ErrorKind` variants mix `ForkFailed`, `LockfileError`, `ValidationError` and
  `LockConflict`. Consider `*Failed` for the noun forms at the next breaking
  release.
- daemonize(1) numbers its own 11-step summary; SPEC numbers 18 steps. Cite
  SPEC step numbers outside the man page.
