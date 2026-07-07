# blivet

[![CI](https://github.com/camercu/blivet/actions/workflows/ci.yml/badge.svg)](https://github.com/camercu/blivet/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/blivet.svg)](https://crates.io/crates/blivet)
[![docs.rs](https://docs.rs/blivet/badge.svg)](https://docs.rs/blivet)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-blue)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field)
[![License](https://img.shields.io/crates/l/blivet.svg)](LICENSE-MIT)

**A correct, full-featured Unix daemon library and CLI for Rust.**

Daemonizing a process *correctly* is deceptively hard: the double-fork dance,
session detachment, signal and fd hygiene, and -- the part most libraries skip
-- telling the launcher whether the daemon actually came up. Unlike `daemon(3)`
or thin wrappers, `blivet` reports post-fork startup failures back to the
launcher over a notification pipe, with `sysexits.h` exit codes. The
shell, `systemd`, or a supervisor sees a real success or failure -- not a
detached process that may have already died during init.

```rust
use blivet::{daemonize, DaemonConfig};

let mut config = DaemonConfig::new();
config.pidfile("/var/run/myapp.pid");

let mut ctx = daemonize(&config)?; // safe: checks single-threaded, then double-forks
ctx.notify_parent()?;              // tell the launcher we're up; it exits 0
// daemon runs here
```

## Why blivet

- **Correct by default.** Mandatory double-fork, `setsid`, signal reset
  (including real-time signals on Linux; SIGPIPE stays ignored so pipe writes
  keep returning errors instead of killing the daemon), signal-mask clear, fd
  close, and `/dev/null` redirect -- the things hand-rolled daemonizers forget.
- **Parent notification.** The launcher blocks until the daemon signals
  readiness or reports an error -- no "did it start?" polling. Forget to call
  `notify_parent()` and the launcher exits non-zero automatically.
- **Split-phase privileges.** `daemonize()` returns while still privileged, so
  you can bind port 80 or `chroot` before `drop_privileges()`.
- **Safe by default.** The checked entry points verify single-threadedness, so
  you write no `unsafe`; the crate's own `unsafe` (libc FFI, `fork`, `setenv`)
  is isolated under `#![deny(unsafe_code)]` and documented. Opt-out variants are
  there when you manage the single-threaded contract yourself.
- **Library and CLI.** A builder API, or the standalone `daemonize` binary
  (`cargo install blivet`) that wraps any program.

## How it works

`daemonize()` double-forks, calls `setsid` to detach from the controlling
terminal, and returns a `DaemonContext` in the grandchild -- your daemon. The
launcher does *not* return; it blocks on a pipe to the grandchild, waiting for
that daemon to report in. In the daemon you then:

1. run fallible init -- bind sockets, open files, connect to dependencies;
2. `chown_paths()`, then `drop_privileges()`, to drop to an unprivileged user;
3. call `notify_parent()`, which writes one readiness byte down the pipe.

```text
launcher ── daemonize() ──► fork → setsid → fork ──► grandchild = your daemon
   ▲                                                     │
   └───────────── readiness / error  (pipe) ─────────────┘
```

That byte releases the launcher, which exits 0. If the daemon instead dies or
calls `report_error()` before notifying, the launcher exits non-zero with the
matching `sysexits.h` code -- so it sees a real result, not a process that
detached and then crashed.

Steps 1-2 must run **single-threaded**: spawn threads, async runtimes, or accept
loops only *after* `drop_privileges()` returns. See [Safety](#safety) for why.

## Install

```sh
cargo install blivet   # the `daemonize` CLI -- verify with `daemonize --version`
cargo add blivet        # the library
```

The crate is `blivet`; the installed binary is `daemonize`. See
[Library](#library) for the API, or [CLI](#cli) for command-line use.

## Library

The minimal example above writes a pidfile and signals readiness; the pidfile
is also locked, so a second instance fails fast instead of clobbering it. A
fuller setup adds a separate lock file, log redirection, and a working
directory. As with
`daemonize(1)`, the defaults are standard daemon behavior -- stdout/stderr go to
`/dev/null` and the working directory becomes `/` -- so use absolute paths and
redirect any output you want to keep:

```rust
use blivet::{daemonize, DaemonConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = DaemonConfig::new();
    config
        .pidfile("/var/run/myapp.pid")
        .lockfile("/var/run/myapp.lock")
        .stdout("/var/log/myapp.out")
        .stderr("/var/log/myapp.err")
        .chdir("/var/lib/myapp");

    let mut ctx = daemonize(&config)?;
    // fallible initialization here (open sockets, load config, …)
    ctx.notify_parent()?; // signal readiness; the launcher exits 0
    Ok(())
}
```

**Split-phase privilege dropping.** Do privileged work (bind port 80, `chroot`,
set rlimits) between `daemonize()` and `drop_privileges()`:

```rust
use blivet::{daemonize, DaemonConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = DaemonConfig::new();
    config.pidfile("/var/run/myapp.pid").user("www-data").group("www-data");

    let mut ctx = daemonize(&config)?;

    // Still root here -- bind a privileged port, chroot, set rlimits…
    // let listener = TcpListener::bind("0.0.0.0:80")?;

    ctx.chown_paths()?;     // transfer file ownership to the target user
    ctx.drop_privileges()?; // then switch user/group
    ctx.notify_parent()?;
    // now running as www-data…
    Ok(())
}
```

**Foreground mode.** For systemd, containers, or debugging: skip forking while
applying all other setup (umask, chdir, signal reset, …). Stdout/stderr stay
inherited unless explicitly redirected with `.stdout()`/`.stderr()`:

```rust
config.foreground(true).close_fds(false); // keep supervisor-passed fds
```

> See [`examples/echo_server.rs`](examples/echo_server.rs) for a complete,
> runnable daemonized TCP echo server with signal-based shutdown and pidfile
> cleanup.

### Entry points

- **`daemonize(&config)`** -- the safe, recommended entry point. It verifies the
  process is single-threaded, then daemonizes. Available on **Linux, macOS,
  FreeBSD, NetBSD, and OpenBSD** (it reads the kernel thread count:
  `/proc/self/status` on Linux, `proc_pidinfo` on macOS, `sysctl` on the BSDs).
  On any other target it is a `#[deprecated]` stub that panics; use the
  unchecked form below.
- **`unsafe { daemonize_unchecked(&config) }`** -- the escape hatch, on all Unix
  platforms. It skips the thread-count check, so *you* must guarantee the
  process is single-threaded.

`DaemonContext::drop_privileges()` mirrors this split: it is safe and checked
(panicking if a user switch is configured while multithreaded), with
`unsafe { drop_privileges_unchecked() }` as the opt-out. See [Safety](#safety).

## Safety

Forking a multithreaded process is unsound: mutexes held by other threads stay
locked forever in the child, deadlocking it. A second thread-unsafe step
follows -- `drop_privileges()` calls `setenv` (`USER`/`HOME`/`LOGNAME`) when
switching users -- so the single-threaded window runs from the fork through
`drop_privileges()` (see [How it works](#how-it-works)). Spawn
threads, an async runtime, or an accept loop only *after* `drop_privileges()`
returns -- or after `daemonize()` returns if you don't switch users.

Both checked entry points read the kernel thread count and panic if violated:
`daemonize()` at the fork, and `drop_privileges()` at its `setenv` (only when a
user switch is configured -- the sole `setenv` path; a group-only switch is not
guarded). `daemonize_unchecked()` and `drop_privileges_unchecked()` are the
`unsafe` opt-outs for callers who manage the contract themselves, or who run on
a target without a thread-count source.

## API reference

Full reference is on [docs.rs](https://docs.rs/blivet); this is the shape of it.

**`DaemonConfig`** -- a builder of infallible `&mut self` setters; validation is
deferred to `validate()`, which `daemonize()` runs for you. Settings: `pidfile`,
`lockfile`/`no_lockfile`, `stdout`/`stderr` (+ `append`), `chdir`, `umask`,
`user`/`group`, `foreground`, `close_fds`, `cleanup_on_drop`, and `env`.
Defaults worth knowing: working directory `/`, stdout/stderr `/dev/null`,
`close_fds` and `cleanup_on_drop` both `true`, and a configured pidfile doubles
as the lockfile -- a second instance fails with `LockConflict` (exit 69) rather
than silently overwriting the pidfile. Point `lockfile()` at a separate path,
or call `no_lockfile()` to write the pidfile unlocked.

**`DaemonContext`** -- returned by a successful `daemonize()`; owns the lockfile
and notification pipe. The methods you reach for most: `notify_parent()`,
`chown_paths()` / `drop_privileges()`, `cleanup()` /
`cleanup_on_term_signals()`, and `report_error()` / `report_error_msg()` /
`notify_parent_or_report()`.

### Errors & exit codes

`DaemonizeError` has sixteen variants covering validation, fork, setsid, lock,
permission, chown, exec, and parent-notify failures, plus a caller-supplied
`Application` variant. Each maps to a `sysexits.h` exit code via `exit_code()`,
so failures reach the shell with a meaningful status:

| Variant            | Exit code   | Meaning                                  |
| ------------------ | ----------- | ---------------------------------------- |
| `ValidationError`  | 64          | Bad config (paths, env keys, overlaps)   |
| `ProgramNotFound`  | 66          | CLI: program missing or not executable (pre-fork check or exec `ENOENT`/`EACCES`) |
| `UserNotFound`     | 67          | User doesn't exist                       |
| `GroupNotFound`    | 67          | Group doesn't exist                      |
| `LockConflict`     | 69          | Lockfile held by another process         |
| `LockfileError`    | 73          | Can't open lockfile                      |
| `PidfileError`     | 73          | Can't write pidfile                      |
| `OutputFileError`  | 73          | Can't open/redirect output file          |
| `ChownError`       | 73          | Can't chown pidfile/lockfile/output file |
| `ForkFailed`       | 71          | `fork()` error                           |
| `SetsidFailed`     | 71          | `setsid()` error                         |
| `ChdirFailed`      | 71          | `chdir()` error                          |
| `PermissionDenied` | 77          | Not root, or setuid/setgid failed        |
| `ExecFailed`       | 71          | CLI: `exec` failed (other than `ENOENT`/`EACCES`) |
| `NotifyFailed`     | 71          | Can't write readiness byte to launcher   |
| `PrivilegesNotDropped` | 70      | user/group set but `drop_privileges()` never called |
| `Application`      | caller's    | App-level failure you report yourself    |

## Recipes

### Pidfile cleanup on signals

When `cleanup_on_drop` is `true` (the default), the pidfile is removed when
`DaemonContext` is dropped. But **`Drop` does not run when the process is killed
by a signal** (`SIGTERM`, `SIGKILL`, …) -- which is how most daemons stop -- so
the pidfile would be left behind. The built-in fix installs async-signal-safe
handlers that remove the pidfile on `SIGINT`/`SIGTERM`, then re-raise so the
process still terminates normally:

```rust
use blivet::{daemonize, DaemonConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = DaemonConfig::new();
    config.pidfile("/var/run/myapp.pid");
    let mut ctx = daemonize(&config)?;

    ctx.cleanup_on_term_signals()?; // or cleanup_on_signals(&[...]) for custom signals
    ctx.notify_parent()?;
    // … daemon work …
    Ok(())
}
```

> **Library-only.** The `daemonize` **CLI** cannot do this: it `exec`s the
> target program, and `exec` resets custom signal handlers to their default
> disposition, so a CLI-launched program must clean up its own pidfile.

If you already run your own signal loop (e.g. for graceful shutdown), you don't
need the built-in handler: let the loop exit, then call `cleanup()` -- or just
let `ctx` drop. The [`signal_hook`](https://crates.io/crates/signal-hook) crate
is one way to drive that loop; `blivet` does not re-export it, so
`cargo add signal_hook` first:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use blivet::{daemonize, DaemonConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = DaemonConfig::new();
    config.pidfile("/var/run/myapp.pid");
    let mut ctx = daemonize(&config)?;
    ctx.notify_parent()?;

    // `flag::register` *sets* the flag when the signal arrives, so start at
    // `false` and loop until it flips.
    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&shutdown))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown))?;

    while !shutdown.load(Ordering::Relaxed) {
        // … daemon work …
    }
    ctx.cleanup(); // or just let ctx drop
    Ok(())
}
```

### Reporting your own failures

If startup work in the privileged init window fails (a socket bind, a database
connect), report it to the launcher with a `sysexits.h` code of your choosing
via `report_error_msg` -- no need to construct a `DaemonizeError` by hand:

```rust
let listener = match TcpListener::bind("0.0.0.0:80") {
    Ok(l) => l,
    // 71 == EX_OSERR; the launcher prints the message and exits with this code.
    Err(e) => ctx.report_error_msg(71, format!("bind failed: {e}")),
};
```

### Propagating exit codes

The `sysexits.h` codes only reach the shell if you use them. The idiomatic
`fn main() -> Result<(), E>` prints the error via `Termination` and exits **1**,
ignoring `exit_code()`. To preserve the codes, drive `exit_code()` yourself:

```rust
use blivet::{daemonize, DaemonConfig, DaemonizeError};

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(e.exit_code() as i32); // e.g. 77 for PermissionDenied
    }
}

fn run() -> Result<(), DaemonizeError> {
    let config = DaemonConfig::new();
    let mut ctx = daemonize(&config)?;
    // … application init …
    // notify_parent() returns DaemonizeError (NotifyFailed, exit 71), so `?`
    // keeps a single error type and preserves the exit code.
    ctx.notify_parent()?;
    Ok(())
}
```

## CLI

The `daemonize` binary wraps any program as a daemon, applying the same setup as
the library:

```sh
# Simplest: daemonize a program
daemonize -- /usr/bin/my-server --port 8080

# Typical service: pidfile, log redirect, working dir, drop to an unprivileged user
daemonize -p /var/run/myapp.pid -o /var/log/myapp.log -c /var/lib/myapp \
  -u www-data -g www-data -- /usr/bin/my-server
```

The launcher blocks until the daemon successfully `exec`s, then exits 0. On
failure (lockfile conflict, permission denied, exec error) it prints the error
to stderr and exits with a `sysexits.h` code -- the same codes the library
returns (see [Errors & exit codes](#errors--exit-codes)). When `-u`/`-g` are
given, the CLI transfers ownership of the pidfile, lockfile, and log files to the
target user/group before dropping privileges, so the daemon can keep writing to
them.

| Flag | Long                | Description |
| ---- | ------------------- | --- |
| `-p` | `--pidfile PATH`    | Write daemon PID to file |
| `-c` | `--chdir PATH`      | Working directory (default: `/`) |
| `-m` | `--umask MODE`      | Process umask in octal (e.g. `022`) |
| `-o` | `--stdout PATH`     | Redirect stdout to file (also sets stderr if `-e` is not given) |
| `-e` | `--stderr PATH`     | Redirect stderr to file (default: stdout path; `.stdout`→`.stderr`, `.out`→`.err`) |
| `-a` | `--append`          | Append to stdout/stderr files instead of truncating |
| `-l` | `--lock PATH`       | Exclusive lockfile (default: pidfile path, if set) |
|      | `--no-lock`         | Do not lock the pidfile (allows multiple instances) |
| `-E` | `--env NAME=VAL`    | Set environment variable (repeatable; a bare `NAME` sets the empty string) |
| `-u` | `--user NAME\|UID`  | Switch to user after daemonizing (requires root) |
| `-g` | `--group NAME\|GID` | Switch to group after daemonizing (requires root) |
| `-f` | `--foreground`      | Stay in foreground (no fork/setsid)                            |
| `-v` | `--verbose`         | Print diagnostic info before daemonizing |

## Minimum supported Rust version

1.85

## Why the name `blivet`?

A [blivet](https://en.wikipedia.org/wiki/Impossible_trident) is the "impossible
pitchfork" optical illusion, also known as the devil's fork, where the prongs
are mysteriously detached from the base. Daemons are created by forking to
detach from their parent terminal.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
