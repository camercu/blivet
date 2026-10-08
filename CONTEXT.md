# blivet

Turns a Unix process into a daemon, correctly, and makes the process that
started it wait for the daemon to report readiness or a failure with a
`sysexits.h` exit code. Ships as a Rust library and as the `daemonize` CLI,
which wraps any program.

## Language

### Processes

**Launcher**:
The original process that called `daemonize()` (or ran the `daemonize` CLI) and blocks until the daemon reports how its start went.
_Avoid_: parent (the launcher is the daemon's grandparent), original process, caller

**Daemon**:
The grandchild process that runs the daemonization sequence and then the application.
_Avoid_: child, grandchild (outside step-level detail), background process

**Intermediate child**:
The process between the two forks; it creates the session and exits 0.
_Avoid_: first child, session leader

**Supervisor**:
An external service manager (systemd, launchd, a container runtime) that runs the program in foreground mode and owns its lifecycle.
_Avoid_: init system, service manager (pick one), process supervisor

### Configuration and setup

**Daemon config**:
The validated set of choices that drives one daemonization (`DaemonConfig`).
_Avoid_: options, settings

**Daemonization sequence**:
The fixed, ordered steps that take a process from launcher to daemon (SPEC steps 1–18).
_Avoid_: daemonization process, setup

**Daemon mode**:
Running the full sequence, with both forks and `setsid`.
_Avoid_: background mode

**Foreground mode**:
Running the sequence without the forks, `setsid`, or notification channel, for a supervisor.
_Avoid_: no-daemon mode, debug mode

**Pidfile**:
A file holding the daemon's process ID.
_Avoid_: PID file, pid file

**Lockfile**:
A file the daemon holds an exclusive `flock` on for its lifetime, so a second instance fails; the pidfile by default.
_Avoid_: lock file, lock (as a noun for the file)

**Single-instance enforcement**:
The guarantee, through the lockfile, that only one daemon per lockfile runs.
_Avoid_: instance locking

**Lock conflict**:
The state where another process already holds the lockfile.
_Avoid_: already running

**Privileged init**:
Caller-supplied work that runs inside the sequence, after the lock and before the identity switch, with the launcher's privileges (`daemonize_with`'s `init`).
_Avoid_: init (bare), init closure, setup hook

**Target identity**:
The user and group the daemon runs as after the identity switch.
_Avoid_: target user (when a group is meant too)

**Identity switch**:
Setting supplementary groups, group, and user to the target identity, and `USER`/`HOME`/`LOGNAME`.
_Avoid_: user switch, privilege drop, drop privileges

**Path ownership transfer**:
Chowning the pidfile, lockfile, and output files to the target identity before the identity switch.
_Avoid_: chown paths (outside the API)

**Output redirect**:
Pointing the daemon's stdout or stderr at a configured file instead of `/dev/null`.
_Avoid_: log file, output file (for the act)

**Single-threaded contract**:
The rule that no other thread exists across the daemonization sequence.
_Avoid_: single-threaded window, thread check

**Checked entry point**:
`daemonize` or `daemonize_with`, which verify the single-threaded contract and panic if it fails.
_Avoid_: safe form

**Unchecked entry point**:
`daemonize_unchecked` or `daemonize_with_unchecked`, which leave the single-threaded contract to the caller.
_Avoid_: unsafe form

### Start outcome

**Notification channel**:
The one-way socket pair over which the daemon tells the launcher how the start went.
_Avoid_: notification pipe (it is not a `pipe(2)`), pipe

**Readiness**:
The daemon's report that it started successfully; the launcher then exits 0.
_Avoid_: ready signal, signal readiness (clashes with Unix signals)

**Exec marker**:
The message saying the daemon is about to `exec` a program; channel close then means success.
_Avoid_: exec pending

**Failure report**:
A message carrying an exit code and text; the launcher prints the text and exits with the code.
_Avoid_: error signal

**Unnotified exit**:
The daemon ending before readiness without a failure report; the launcher exits 1.
_Avoid_: silent death

**Exit code**:
The `sysexits.h` status a failure maps to, or the caller's chosen code for an application error.
_Avoid_: exit status (for the mapped value), return code

**Application error**:
A failure the caller reports with its own exit code (`DaemonizeError::Application`).
_Avoid_: custom error, user error

**Program handoff**:
Replacing the daemon with another program by `execvp`, keeping the lock (`DaemonContext::exec`, the CLI's last step).
_Avoid_: spawn, run program

### Shutdown

**Pidfile cleanup**:
Removing the pidfile when the daemon ends; never removes a standalone lockfile.
_Avoid_: cleanup (bare), teardown

**Stale pidfile**:
A pidfile left on disk after its daemon ended.
_Avoid_: leftover pidfile, orphan pidfile

**Termination signal**:
`SIGINT` or `SIGTERM`, the signals that normally stop a daemon.
_Avoid_: kill signal, term signal

## Event model

Two parts share one timeline: the **daemon** side (library API, CLI) and the
**launcher** side (no public API; it reacts to the notification channel).

### Library

| Command | Events | Standard name | Code |
|---|---|---|---|
| Configure the daemon | — | daemonize(1) options | `DaemonConfig::new`, `pidfile`, `chdir`, `umask`, `stdout`, `stderr`, `append`, `lockfile`, `no_lockfile`, `user`, `group`, `foreground`, `close_fds`, `cleanup_on_drop`, `chown_paths`, `env` |
| Validate the config | Config validated; config rejected | EX_USAGE (64) | `DaemonConfig::validate`, `ValidationError` |
| Daemonize | Launcher forked; session created; daemon forked; umask set; working directory changed; standard streams redirected; lockfile locked; pidfile written; signals reset; signal mask cleared; environment set; output redirected; inherited fds closed; target identity resolved; paths chowned; identity switched; context returned | daemon(7) "SysV daemon" steps; Stevens APUE §13.3 coding rules; `daemon(3)` | `daemonize`, `daemonize_unchecked` |
| Daemonize with privileged init | as Daemonize, plus init ran; init failed; init left threads | daemon(7) "drop privileges" step | `daemonize_with`, `daemonize_with_unchecked` |
| | Fork failed | `fork(2)` EAGAIN → EX_OSERR | `ForkFailed` |
| | Session creation failed | `setsid(2)` → EX_OSERR | `SetsidFailed` |
| | Working directory change failed | `chdir(2)` → EX_OSERR | `ChdirFailed` |
| | Lockfile open failed | EX_CANTCREAT | `LockfileError` |
| | Lock conflict | APUE "single-instance daemon" → EX_UNAVAILABLE | `LockConflict` |
| | Pidfile write failed | EX_CANTCREAT | `PidfileError` |
| | Output redirect failed | EX_CANTCREAT | `OutputFileError` |
| | Chown failed | EX_CANTCREAT | `ChownError` |
| | User not found; group not found | EX_NOUSER | `UserNotFound`, `GroupNotFound` |
| | Identity switch refused | EX_NOPERM | `PermissionDenied` |
| | System call failed | EX_OSERR | `SystemError` |
| | Init left threads | EX_SOFTWARE | `InitLeftThreads` |
| | Sequence panicked | EX_SOFTWARE (70) | — (caught, reported) |
| Notify readiness | Readiness sent; notify failed | systemd `READY=1` "readiness notification" | `DaemonContext::notify_parent`, `NotifyFailed` |
| Notify readiness or exit | Readiness sent; notify failed → daemon exits 71 | — | `DaemonContext::notify_parent_or_report` |
| Report a failure | Failure reported | daemon(7) step 15 (original process exits with error) | `DaemonContext::report_error`, `report_error_msg` |
| Make an application error | — | sysexits code chosen by caller | `DaemonizeError::application`, `WithExitCode::with_exit_code`, `Application` |
| Hand off to a program | Exec marker sent; program exec'd; program not found; exec failed | `execvp(3)`; daemonize(1) | `DaemonContext::exec`, `ProgramNotFound`, `ExecFailed` |
| Clean up the pidfile | Pidfile removed | — | `DaemonContext::cleanup` |
| Clean up on termination signals | Cleanup handlers installed; handler refused (signal already handled or ignored) | — | `cleanup_on_term_signals`, `cleanup_on_signals` |
| Set cleanup on drop | — | — | `DaemonContext::set_cleanup_on_drop` |
| | Context dropped unnotified | — | `Drop for DaemonContext` (exit 1) |

### CLI

| Command | Events | Standard name | Code |
|---|---|---|---|
| Daemonize a program | Program resolved; program not found; then the library events through Program handoff | Debian `daemonize(1)` | `daemonize [OPTIONS] -- program [args]` |
| Print help; print version | — | GNU `--help`/`--version` | `-h`, `-V` |

Flag ↔ config: `-p`→`pidfile`, `-c`→`chdir`, `-m`→`umask`, `-o`→`stdout`,
`-e`→`stderr`, `-a`→`append`, `-l/--lock`→`lockfile`,
`--no-lock`→`no_lockfile`, `-E`→`env`, `-u`→`user`, `-g`→`group`,
`-f`→`foreground`. `-v` (verbose diagnostics) has no library counterpart.
`close_fds`, `cleanup_on_drop`, `chown_paths` have no CLI flag.

### Launcher side

| Command | Events | Standard name | Code |
|---|---|---|---|
| Wait for the daemon | Readiness received; exec marker received; failure report received; channel closed | daemon(7) step 15 | internal (`notify` module) |

Views: **Exit code** (`DaemonizeError::exit_code`), **Lockfile fd**
(`DaemonContext::lockfile_fd`), thread count (internal).

Automations:
- readiness received → launcher exits 0.
- failure report received → launcher prints the message, exits with its code.
- channel closed after exec marker → launcher exits 0.
- channel closed with no marker → launcher exits 1.
- step failed after first fork → daemon removes pidfile, sends failure report, `_exit`s.
- init failed → pidfile removed, failure report sent.
- context dropped unnotified → pidfile removed, failure report "daemon exited without signaling readiness" (exit 1).
- context dropped after readiness, cleanup on drop set → pidfile removed.
- termination signal delivered, handlers installed → pidfile removed, default disposition restored, signal re-raised.

Unmapped: none of the 4 public types (`DaemonConfig` → Daemon config,
`DaemonContext` → the daemon's handle after the sequence, `DaemonizeError` →
failure kinds, `WithExitCode` → Application error).

## Relationships

- A **Launcher** waits on exactly one **Daemon** through one **Notification channel**.
- A **Daemon** sends at most one of: **Readiness**, **Exec marker** then success, or **Failure report**; sending none is an **Unnotified exit**.
- A **Daemon config** names zero or one **Pidfile** and zero or one **Lockfile**; with no explicit **Lockfile**, the **Pidfile** is the **Lockfile**.
- A **Lockfile** enforces **Single-instance enforcement** for every daemon that names it.
- **Privileged init** runs at most once per **Daemon**, after the **Lockfile** is locked and before the **Identity switch**.
- **Path ownership transfer** precedes the **Identity switch** and covers the **Pidfile**, **Lockfile**, and redirected output files.
- Every failure maps to exactly one **Exit code**; several failures share one code.
- **Foreground mode** has no **Launcher** and no **Notification channel**; failures return to the caller.

## Example dialogue

> **Dev:** "When the **Daemon** calls `notify_parent()`, does its actual parent see it?"
> **Domain expert:** "No. Its parent is the **Intermediate child**, already gone. The message goes to the **Launcher**, the grandparent, which exits 0 on **Readiness**."
> **Dev:** "If **Privileged init** fails to bind port 80, who learns the code?"
> **Domain expert:** "The **Launcher**. The **Daemon** removes its **Pidfile**, sends a **Failure report** with the **Exit code** you chose via `with_exit_code`, and exits."
> **Dev:** "And a second instance with the same **Pidfile**?"
> **Domain expert:** "It hits a **Lock conflict** before its **Privileged init** runs, so it never binds anything. The **Launcher** exits 69."
> **Dev:** "Under systemd I'd use **Foreground mode**. Is there still a **Launcher**?"
> **Domain expert:** "No. The **Supervisor** owns the process; there is no **Notification channel**, and failures come back as `Err` to your code."

## Flagged ambiguities

- "parent" means the **Launcher** in `notify_parent`, man page "parent exits 0", and SPEC; it is the daemon's grandparent. Open: settled API name `notify_parent`. Recommend keeping "Launcher" in prose; consider `notify_ready` at next breaking release.
- "signal" means both a Unix signal and a channel message (`signal_ready`, "signal readiness", "signals the error to the parent"). Resolved in prose: **Readiness**, **Failure report**; "signal" only for Unix signals.
- "notification pipe" names a socket pair (SPEC: "The name stays"). Open: settled in SPEC. Recommend **Notification channel**, the term ADR 0006 already uses.
- "init" means **Privileged init**, an init system (SPEC: "original process (or init system)"), init scripts (man), and "application init". Resolved: **Privileged init** for the closure, **Supervisor** for the others.
- "lock" vs "lockfile": API `lockfile()`/`no_lockfile()`, CLI `--lock`/`--no-lock`, man page "lock file"/"PID file". Open: CLI flags are settled. Recommend **Lockfile**/**Pidfile** (Debian daemonize(1) spelling) everywhere in prose.
- "user switch", "identity switch", "switch user", "drop privileges" (pre-ADR-0004 API) name one step. Resolved: **Identity switch**.
- `DaemonizeError` variant names mix forms: `ForkFailed` (event), `LockfileError`/`PidfileError`/`ChownError`/`SystemError` (noun + Error), `ValidationError` (repeats "Error" inside the error enum), `LockConflict` (state). Open: public API. Recommend `*Failed` for the noun forms at next breaking release.
- Defect: `notify_parent_or_report`'s name and SPEC promise a report "to the parent"; its rustdoc says the spent channel means no message reaches the launcher, and stderr is `/dev/null` in daemon mode. Only the exit status reports.
- Defect: man page EXIT STATUS omits 1 (unnotified exit) and 70 (panic), which the CLI can return.
- Doc drift: man page numbers 11 sequence steps, SPEC 18; "step 9" (chown) in the man page is SPEC step 16. Readers cross-referencing step numbers get the wrong step.
- "cleanup" removes only the pidfile. Resolved: **Pidfile cleanup** in prose; API names stay.
