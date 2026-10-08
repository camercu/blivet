# blivet

[![CI](https://github.com/camercu/blivet/actions/workflows/ci.yml/badge.svg)](https://github.com/camercu/blivet/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/blivet.svg)](https://crates.io/crates/blivet)
[![docs.rs](https://docs.rs/blivet/badge.svg)](https://docs.rs/blivet)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-blue)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field)
[![License](https://img.shields.io/crates/l/blivet.svg)](LICENSE-MIT)

**A correct, full-featured Unix daemon library and CLI for Rust.**

Backgrounding a process is easy. Doing it correctly is not. A daemon that
forks, detaches, and then fails mid-init looks like success, because the
launcher has already exited 0. `blivet` makes the launcher wait on a pipe until
the daemon reports readiness or a failure, with a `sysexits.h` exit code. It
also handles signals, fds, privileges, and output redirects by default, with no
`unsafe` in your code.

```rust,no_run
use blivet::{daemonize, DaemonConfig, DaemonizeError};

fn main() -> Result<(), DaemonizeError> {
    let mut config = DaemonConfig::new();
    config.pidfile("/var/run/myapp.pid");

    let mut ctx = daemonize(&config)?; // safe: checks single-threaded, then double-forks
    ctx.notify_parent()?;              // tell the launcher we're up; it exits 0
    // daemon runs here
    Ok(())
}
```

## Why blivet

- **Correct by default.** Double-fork, `setsid`, reset signal dispositions
  (real-time signals too, on Linux and Android), a cleared signal mask, closed
  fds, and `/dev/null` on stdio. `SIGPIPE` keeps the caller's disposition:
  ignored in a normal Rust binary, so a write to a closed pipe returns an error
  instead of killing the daemon.
- **No `unsafe` in your code.** `daemonize()` and `daemonize_with()` check
  that the process is single-threaded before the steps that need it. Inside
  the crate, `#![deny(unsafe_code)]` makes each `unsafe` block opt in.
- **Privileged init, once.** `daemonize_with()` runs your init after the
  pidfile lock and while still root, so you can bind port 80 or open root-only
  files, and a second instance fails before its init runs. Then it drops to the
  configured user.
- **Library and CLI.** A builder with infallible setters and deferred
  validation, or the `daemonize` binary, which wraps any program.

## How it works

`daemonize()` double-forks, calls `setsid` to detach from the controlling
terminal, and returns a `DaemonContext` in the grandchild, which is your
daemon. The launcher does not return. It blocks on a pipe until the daemon
reports in. Inside the call, in the daemon, blivet takes the pidfile lock, runs
your privileged init if you passed one to `daemonize_with()`, then chowns the
pidfile, lockfile, and log files (regular files only, never `/dev/null`) to the
configured user and switches to it. Your
daemon then calls `notify_parent()`, which writes a readiness message down the
pipe.

```text
launcher
│ daemonize(): fork, setsid, fork
├───────────────────┐
│ waits on pipe     ▼
│                 daemon
│                   │ lock, init
│                   │ switch user
│◄──────────────────┘ notify_parent()
▼
exits 0, or the error's code
```

That message releases the launcher, which exits 0. If the daemon calls
`report_error()`, the launcher exits with that error's `sysexits.h` code. If the
daemon panics during daemonizing, in `init` too, the launcher exits 70 with the
panic text. If the daemon ends any other way before notifying, the launcher
exits 1. That covers an early return, a later panic, a kill by a signal, and a
call to `std::process::exit`.

The daemonize call must run **single-threaded**. [Safety](#safety) explains
why.

## Install

```sh
cargo install blivet   # the `daemonize` CLI -- verify with `daemonize --version`
cargo add blivet       # the library
cargo add blivet --no-default-features   # the library without the CLI's parser
```

The crate is `blivet`; the installed binary is `daemonize`. See
[Library](#library) for the API, or [CLI](#cli) for command-line use.

The `cli` feature is on by default and carries `clap`, which the `daemonize`
binary needs and the library does not. Turning it off takes a library-only
build from 24 crates to 11; `cargo install blivet` is unaffected either way.

## Library

The minimal example above writes a locked pidfile, so a second instance fails
with `LockConflict` (exit 69) instead of overwriting it. Call `lockfile()` to
lock a separate path, or `no_lockfile()` to write the pidfile unlocked. The
other defaults match `daemonize(1)`. Stdout and stderr go to `/dev/null`, the
working directory is `/`, and inherited fds are closed. Use absolute paths, and
redirect any output you want to keep. A fuller setup:

```rust,no_run
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

**Privileged init.** With `user()` or `group()` set, the daemon switches to
them before `daemonize()` returns. Pass work that needs root (bind port 80,
open root-only files, set rlimits) to `daemonize_with()`. It runs after the
lock, so only one instance runs it, and its value comes back beside the
context:

```rust,no_run
use blivet::{daemonize_with, DaemonConfig, WithExitCode};
use std::net::TcpListener;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = DaemonConfig::new();
    config.pidfile("/var/run/myapp.pid").user("www-data").group("www-data");

    let (mut ctx, listener) = daemonize_with(&config, || {
        // Still root, holding the lock.
        TcpListener::bind("0.0.0.0:80").with_exit_code(71) // EX_OSERR on failure
    })?;

    // Now running as www-data, with the socket open.
    ctx.notify_parent()?;
    # drop(listener);
    Ok(())
}
```

An error from `init` reaches the launcher with its exit code, and the pidfile
is removed. `with_exit_code(code)` gives any
failed `Result` that code.

A `chroot` in `init` is not supported (see `docs/adr/0005`): blivet still
chowns the right files and sets the right groups, but the `chdir` into the
jail and the pidfile, which cleanup can no longer reach by path, are yours.

**Foreground mode.** For systemd, containers, or debugging: skip forking while
applying all other setup (umask, chdir, signal reset, …). Stdout/stderr stay
inherited unless explicitly redirected with `.stdout()`/`.stderr()`:

```rust,ignore
config.foreground(true).close_fds(false); // keep supervisor-passed fds
```

> See [`examples/echo_server.rs`](examples/echo_server.rs) for a complete,
> runnable daemonized TCP echo server with signal-based shutdown and pidfile
> cleanup.

## Safety

Forking a multithreaded process is unsound. Mutexes that other threads hold stay
locked forever in the child, so the child deadlocks. Switching user has the same
hazard, because it calls `setenv` for `USER`, `HOME`, and `LOGNAME`. Both happen
inside the daemonize call, so spawn threads, an async runtime, or an accept loop
only after it returns. An `init` may use threads only if it joins them before
it returns.

The checked entry points enforce this. `daemonize()` and `daemonize_with()`
read the kernel thread count and panic unless exactly one thread is running.
With a user switch, `daemonize_with()` checks again after `init` and, if `init`
left threads running, fails with `InitLeftThreads` (exit 70), which reaches the
launcher. A group-only switch calls no `setenv`, so that second check is
skipped. The `unsafe` forms
`daemonize_unchecked()` and `daemonize_with_unchecked()` skip the checks, for
callers who manage the contract themselves. They are also the only entry points
on a target where the thread count can't be read (see
[Platform support](#platform-support)).

## API reference

[docs.rs](https://docs.rs/blivet) has the full reference.

### Errors & exit codes

`DaemonizeError` covers validation, fork, setsid, lock, permission, chown, exec,
and parent-notify failures, plus a caller-supplied `Application` variant. Each
variant maps to a `sysexits.h` exit code via `exit_code()`, so failures reach
the shell with a meaningful status:

| Variant            | Exit code   | Meaning                                  |
| ------------------ | ----------- | ---------------------------------------- |
| `ValidationError`  | 64          | Bad config (paths, env keys, overlaps)   |
| `ProgramNotFound`  | 66          | Program missing or not executable (the CLI's pre-fork check, or `exec` failing with `ENOENT`/`EACCES`) |
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
| `SystemError`      | 71          | Syscall failed (notification pipe, `/dev/null` open or `dup2`, `sigaction`, `sigprocmask`, `getrlimit`, thread count around init) |
| `PermissionDenied` | 77          | Not root, or initgroups/setgid/setuid failed |
| `ExecFailed`       | 71          | `exec` failed (other than `ENOENT`/`EACCES`) |
| `NotifyFailed`     | 71          | Can't write readiness to launcher        |
| `InitLeftThreads`  | 70          | `daemonize_with` init left threads running before a user switch |
| `Application`      | caller's    | App-level failure you report yourself    |

## Recipes

### Pidfile cleanup on signals

When `cleanup_on_drop` is `true` (the default), the pidfile is removed when
`DaemonContext` is dropped. But **`Drop` does not run when a signal kills the
process**, and most daemons stop that way (`SIGTERM`, `SIGKILL`, …). The pidfile
stays behind. The built-in fix installs async-signal-safe
handlers that remove the pidfile on `SIGINT`/`SIGTERM`, then re-raise so the
process still terminates normally:

```rust,no_run
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
need the built-in handler. Let the loop exit, then call `cleanup()` or let
`ctx` drop. The [`signal-hook`](https://crates.io/crates/signal-hook) crate
is one way to drive that loop; `blivet` does not re-export it, so
`cargo add signal-hook` first. Pick one approach per signal:
`cleanup_on_signals` refuses a signal that already has a handler, such as one
`signal_hook` registered, or that is ignored, rather than silently replacing it.

```rust,no_run
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

Inside `daemonize_with()`'s init, use `.with_exit_code(code)` or return
`DaemonizeError::application(code, message)`. After the call, when startup work
that needs no privileges fails (a database connect), report it to the launcher
with a `sysexits.h` code of your choosing via `report_error_msg`, without
building a `DaemonizeError` by hand:

```rust,ignore
let db = match Database::connect(&url) {
    Ok(db) => db,
    // 69 == EX_UNAVAILABLE; the launcher prints the message and exits with it.
    Err(e) => ctx.report_error_msg(69, format!("database connect failed: {e}")),
};
```

### Handing off to another program

`ctx.exec(program, argv)` replaces the daemon with another program, as the
`daemonize` CLI does. The program keeps the lock and starts with the default
`SIGPIPE`. The launcher exits 0 once the exec succeeds, or with the exec's
error (66 for a missing program, 71 otherwise) if it fails.

### Propagating exit codes

The `sysexits.h` codes only reach the shell if you use them. The idiomatic
`fn main() -> Result<(), E>` prints the error via `Termination` and exits **1**,
ignoring `exit_code()`. To preserve the codes, drive `exit_code()` yourself:

```rust,no_run
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
to stderr and exits with the same `sysexits.h` code the library returns (see
[Errors & exit codes](#errors--exit-codes)). When `-u`/`-g` are
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
| `-f` | `--foreground`      | Stay in foreground (no fork/setsid) |
| `-v` | `--verbose`         | Print diagnostic info before daemonizing |

## Platform support

| Tier | What CI proves | Platforms |
| --- | --- | --- |
| **Supported** | The library tests pass on x86_64 (macOS: arm64) | Linux, Android, macOS, FreeBSD, NetBSD, OpenBSD |
| **Cross-checked** | The crate compiles; no tests run | `aarch64-linux-android` |
| **Best-effort** | The crate compiles, and the CLI runs on illumos | Other Unix targets that `nix` supports |

CI builds no other architecture of a Supported OS. aarch64 Linux, for example,
uses the same code as x86_64 Linux, but nothing checks it.

On a best-effort target, `daemonize()` and `daemonize_with()` are
`#[deprecated]` stubs that panic. Call `daemonize_unchecked()` and
`daemonize_with_unchecked()` instead. A crate that also builds there can gate
the checked call on `target_os`, as the [crate front page](https://docs.rs/blivet)
and `examples/echo_server.rs` do. Two Unix targets `nix` supports do not
compile: Solaris (`x86_64-pc-solaris`), because `nix` lacks `Flock` there, and
Redox (`x86_64-unknown-redox`), which also lacks `initgroups` and `faccessat`. A
non-Unix target fails with one `compile_error!` that names it.

Android needs API level 24 (Android 7.0) or later. There, the pre-fork check
that the pidfile, lockfile, and output directories are writable tests the real
UID, not the effective UID, because bionic rejects `AT_EACCESS`. CI runs the
Android library tests against bionic in a Linux container. An emulator runs
the CLI on a real Android kernel, but that job does not block a release.

[docs/SPEC.md](docs/SPEC.md#platform-support) has the details.

## Minimum supported Rust version

1.85

## Why the name `blivet`?

A [blivet](https://en.wikipedia.org/wiki/Impossible_trident) is the "impossible
fork" optical illusion, also known as the devil's fork, where the prongs
are mysteriously detached from the base. Daemons are created by forking to
detach from their parent terminal.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
