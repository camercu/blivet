# 0004. Privileged init runs inside the daemonize call

- Status: Accepted (proceed)
- Date: 2026-09-30

## Context

Through 0.14, privilege dropping was split-phase. `daemonize(&config)` forked,
took the pidfile lock, and returned a `DaemonContext` still running as root.
The caller did its privileged work (bind port 80, `chroot`, open root-only
files), then called `ctx.drop_privileges()`, which chowned the pidfile,
lockfile, and logs to the target user, switched group and user, and set
`USER`, `HOME`, and `LOGNAME`. Then it called `ctx.notify_parent()`.

The split cost three concepts: `drop_privileges`, its `unsafe`
`drop_privileges_unchecked` twin, and a `PrivilegesNotDropped` error for a
caller that forgot the drop. It also meant two single-threaded windows the
caller had to bracket: the fork, and `drop_privileges`' `setenv`.

The question was whether the split earns that cost. Most privileged work could
run before `daemonize()`. One benefit could not move: work done after
`daemonize()` runs only in the instance that holds the pidfile lock. Before
the lock, a second instance runs its init side effects (binding, truncating a
file, replacing a socket) and only then fails, often with the wrong error: "port
in use" rather than "already running".

## Decision

Privileged init is a closure that `daemonize_with` runs inside the daemonize
sequence:

```rust
pub fn daemonize_with<T>(
    config: &DaemonConfig,
    init: impl FnOnce() -> Result<T, DaemonizeError>,
) -> Result<(DaemonContext, T), DaemonizeError>;
```

In the daemon process the sequence becomes: fork, `setsid`, fork, lock, write
the pidfile, reset signals, redirect, close inherited fds, **run `init`**, then
chown the configured paths and switch group and user (with its `setenv`), then
return the context and `init`'s value. Anything `init` opens (a bound
listener) comes back as `T`; it is opened after the fd sweep, so nothing
closes it. An `init` error goes to the launcher with its `sysexits.h` code
through the same path as every other post-fork error, and the pidfile is
removed.

- The closure takes no argument. A `&DaemonContext` argument offered almost
  nothing useful inside init.
- `daemonize(&config)` is `daemonize_with` with no init. When `user()` or
  `group()` is set it switches identity before it returns. Code that needs
  root after the switch point must move that work into `init`; if it does not,
  the first privileged call fails loudly at startup (for example `EACCES` from
  `bind`). Code written for the split-phase API does not compile, because
  `drop_privileges` is gone.
- The checked forms verify the thread count at entry and again after `init`
  when a user switch is configured, before the `setenv`. There is one
  single-threaded window, and it is the call.
- `drop_privileges`, `drop_privileges_unchecked`, and
  `DaemonizeError::PrivilegesNotDropped` are removed. Forgetting the drop can
  no longer happen.

## Alternatives

Five throwaway spikes answered this, each porting the same echo-server example
and running the same harness: start the daemon, start a second instance on the
same pidfile, check the echo, stop it.

- **Keep split-phase.** It passes every check but keeps the three concepts, two
  windows, and a thread check that fires after the privileged side effects
  have run.
- **One call, init before `daemonize`, plus a `keep_fd` API** so pre-bound
  listeners survive the fd sweep. It fails the lock-first check: the second
  instance exited 71 (`EX_OSERR`, "Address already in use") instead of 69
  (`EX_UNAVAILABLE`, blivet's lock conflict), and on a free
  port its init side effects ran before it hit the lock. Forgetting `keep_fd`
  reported success for a daemon that then died.
- **`config.user(name, || init)`**, returning a value that `.daemonize()`
  consumes. It passes every check, but privileged work belongs to the
  locked, still-root window, not to the identity: a daemon with no switch
  (`chroot` only) still needs init. That left two ways to run init, a
  branch per runtime-chosen identity, a new type with four methods, and user
  and group living outside the config.
- **`daemonize()` refuses a config with a user set**, forcing
  `daemonize_with(&config, || Ok(()))` for a switch without init. It closes
  the loud-at-startup misuse above with a runtime check, at the cost of closure
  noise for the CLI and every switch-only daemon. Declined by the owner.

An unanchored clean-room spike, given only the problem, arrived at the same
closure shape as the directed one.

## Consequences

- Breaking change: `drop_privileges`, `drop_privileges_unchecked`, and
  `PrivilegesNotDropped` (exit 70, `EX_SOFTWARE`) are removed, and `daemonize()`
  with a user set now switches before returning.
- `daemonize_unchecked` gets an init-taking twin. The CLI calls it with no init
  and loses its separate `drop_privileges_unchecked` block.
- Tests that drove the full sequence with a user configured, unprivileged, can
  no longer do so: the switch now happens inside the sequence. The
  privileged tier (`just docker-test`, which runs the suite as root in a
  container) carries those.
- `docs/SPEC.md` (the split-phase requirements and the `DaemonContext` API),
  the README (How it works, Safety, the split-phase example, the exit-code
  table), and the crate docs change with the build.

## Open items for the build

- `?` on an `io::Error` inside `init` does not compile; every fallible call
  needs `map_err` to a `DaemonizeError`. Add a short way to attach an exit
  code.
- The thread-check panic message reads badly for the post-init check ("Call
  daemonize_with (after privileged init) before …").
- Resolve the target user and group before `init`, so a misspelled user fails
  before init's side effects run.
- Replace `DaemonConfig::exec_notifies` in the same build. Today that flag
  tells the launcher to read the pipe closing with no message as success,
  because a successful `exec` closes it. A daemon that will
  `exec` should say so on the pipe (an explicit marker byte) through a new
  `DaemonContext::exec`, instead of a flag set at fork time that makes a death
  before `exec` read as success.
- A panic inside `init` reaches the launcher as the generic "unnotified"
  failure, not the panic text. That matches the old `drop_privileges`.
