# blivet

[![CI](https://github.com/camercu/blivet/actions/workflows/ci.yml/badge.svg)](https://github.com/camercu/blivet/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/blivet.svg)](https://crates.io/crates/blivet)
[![docs.rs](https://docs.rs/blivet/badge.svg)](https://docs.rs/blivet)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-blue)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field)
[![License](https://img.shields.io/crates/l/blivet.svg)](LICENSE-MIT)

A correct, full-featured Unix daemon library and CLI for Rust.

Daemonizing a process *correctly* is deceptively hard: the double-fork dance,
session detachment, signal and fd hygiene, and -- the part most libraries skip
-- telling the launcher whether the daemon actually came up. Unlike `daemon(3)`
or thin wrappers, `blivet` reports post-fork startup failures back to the
launching process over a notification pipe, with `sysexits.h` exit codes. The
shell, `systemd`, or a supervisor sees a real success or failure -- not a
detached process that may have already died during init.

**Why this crate?**

- **Correct by default.** Mandatory double-fork, `setsid`, signal reset
  (including real-time signals on Linux), signal mask clear, fd close,
  `/dev/null` redirect -- the things most hand-rolled daemonizers forget.
- **Parent notification.** The calling process blocks until the daemon signals
  readiness or reports an error. No more "did it start?" polling.
- **Split-phase privilege dropping.** `daemonize()` returns while still
  privileged, giving you a window for operations like binding privileged ports
  before calling `drop_privileges()`.
- **Fail-safe drop.** If you forget to call `notify_parent()`, the parent
  exits non-zero automatically.
- **Safe by default.** `daemonize()` verifies the process is single-threaded
  (forking with threads is unsound) before forking -- no `unsafe` needed. An
  `unsafe` escape hatch, `daemonize_unchecked()`, is there when you need it.
- **Unsafe contained.** `#![deny(unsafe_code)]` at the crate root. Raw
  libc/syscall `unsafe` is isolated in one module (`unsafe_ops`) behind safe
  wrappers; the only `unsafe` elsewhere is the `fork()` call itself.
- **Library and CLI.** Use it as a Rust library with a builder API, or as a
  standalone `daemonize` binary (installed by `cargo install blivet`) that
  wraps any program.

**Why the name `blivet`?**

A [blivet](https://en.wikipedia.org/wiki/Impossible_trident) is the "impossible
pitchfork" optical illusion, also known as the devil's fork, where the prongs
are mysteriously detached from the base. Daemons are created by forking to
detach from their parent terminal.

## Contents

- [How it works](#how-it-works)
- [Install](#install)
- [CLI quickstart](#cli-quickstart) -- [flags](#cli-flags)
- [Library quickstart](#library-quickstart) -- [entry points](#entry-points),
  [split-phase privilege dropping](#split-phase-privilege-dropping),
  [foreground mode](#foreground-mode)
- [Safety: the single-threaded rule](#safety-the-single-threaded-rule)
- [API overview](#api-overview) -- [config](#daemonconfig),
  [the call](#the-daemonize-call), [context](#daemoncontext),
  [errors & exit codes](#errors--exit-codes)
- [Recipes](#recipes) -- [pidfile cleanup](#pidfile-cleanup-on-signals),
  [reporting your own failures](#reporting-your-own-failures),
  [propagating exit codes](#propagating-exit-codes)
- [Minimum supported Rust version](#minimum-supported-rust-version)
- [License](#license)

## How it works

`daemonize()` double-forks, detaches from the controlling terminal, and keeps a
pipe open back to the launching process so it can report readiness or failure:

```text
caller (shell / systemd / supervisor)            daemon
  │
  │  daemonize(&config)
  ├──────────── fork ───────────► child ── setsid ── fork ──► grandchild (daemon)
  │                                                              │ privileged init
  │                                                              │ drop_privileges()
  │   readiness / error  ◄────── notification pipe ──────────────┤ notify_parent()
  ▼
exits 0 on success, or prints the error
and exits with its sysexits.h code on failure
```

The grandchild is the daemon. Do your fallible initialization — bind sockets,
open files, connect to dependencies — *after* `daemonize()` returns but *before*
`notify_parent()` (and before `drop_privileges()` if you switch users): a failure
in that window is reported back to the parent, which exits non-zero, whereas once
you call `notify_parent()` the parent has already exited 0. Hold off on spawning
threads or starting an async runtime until that single-threaded startup window
closes — see [Safety](#safety-the-single-threaded-rule).

## Install

```sh
cargo install blivet
```

This installs the **`daemonize`** command (the crate is `blivet`; the binary is
`daemonize`). Verify with `daemonize --version`.

Or add the library to your project:

```sh
cargo add blivet
```

## CLI quickstart

Daemonize any program:

```sh
# Simplest: daemonize a program
daemonize -- /usr/bin/my-server --port 8080

# Typical service: pidfile, log redirection (stderr mirrors stdout),
# working directory, and drop to an unprivileged user
daemonize \
  -p /var/run/myapp.pid \
  -o /var/log/myapp.log \
  -c /var/lib/myapp \
  -u www-data -g www-data \
  -- /usr/bin/my-server
```

See [CLI flags](#cli-flags) for the rest (foreground mode, split stdout/stderr,
a separate lockfile, environment variables).

The parent process blocks until the daemon successfully calls `exec`, then
exits 0. If anything fails (lockfile conflict, permission denied, exec error),
the parent prints the error to stderr and exits with a `sysexits.h` code.

When `-u`/`-g` are specified, the CLI transfers ownership of the pidfile,
lockfile, and log files to the target user/group before dropping privileges,
so the daemon can continue to write to them after the switch.

### CLI flags

| Flag | Long                | Description |
| ---- | ------------------- | --- |
| `-p` | `--pidfile PATH`    | Write daemon PID to file |
| `-c` | `--chdir PATH`      | Working directory (default: `/`) |
| `-m` | `--umask MODE`      | Process umask in octal (e.g. `022`) |
| `-o` | `--stdout PATH`     | Redirect stdout to file (also sets stderr if `-e` is not given) |
| `-e` | `--stderr PATH`     | Redirect stderr to file (default: stdout path; `.stdout`→`.stderr`, `.out`→`.err`) |
| `-a` | `--append`          | Append to stdout/stderr files instead of truncating |
| `-l` | `--lock PATH`       | Exclusive lockfile (default: pidfile path, if set) |
| `-E` | `--env NAME=VAL`    | Set environment variable (repeatable) |
| `-u` | `--user NAME\|UID`  | Switch to user after daemonizing (requires root) |
| `-g` | `--group NAME\|GID` | Switch to group after daemonizing (requires root) |
| `-f` | `--foreground`      | Stay in foreground (no fork/setsid)                            |
| `-v` | `--verbose`         | Print diagnostic info before daemonizing |

## Library quickstart

The smallest useful daemon -- write a pidfile, signal readiness, run:

```rust
use blivet::{daemonize, DaemonConfig};

let mut config = DaemonConfig::new();
config.pidfile("/var/run/myapp.pid");

let mut ctx = daemonize(&config)?;   // safe: verifies single-threaded, then double-forks
ctx.notify_parent()?;                // tell the launcher we're up; it exits 0
// daemon runs here
```

A fuller setup -- lock file, log redirection, working directory. As with
`daemonize(1)`, the defaults are standard daemon behavior: stdout/stderr go to
`/dev/null` and the working directory becomes `/`, so use absolute paths and
redirect any output you want to keep:

```rust
use blivet::{DaemonConfig, daemonize};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = DaemonConfig::new();
    config
        .pidfile("/var/run/myapp.pid")
        .lockfile("/var/run/myapp.lock")
        .stdout("/var/log/myapp.out")
        .stderr("/var/log/myapp.err")
        .chdir("/var/lib/myapp");

    let mut ctx = daemonize(&config)?;

    // Application initialization goes here (open sockets, load config, etc.)

    ctx.notify_parent()?;  // signal readiness; the parent exits 0

    // Daemon continues running...
    Ok(())
}
```

> **Tip:** see [`examples/echo_server.rs`](examples/echo_server.rs) for a
> complete, runnable daemonized TCP echo server with clean signal-based
> shutdown and pidfile removal.

### Entry points

There are two ways to daemonize:

- **`daemonize(&config)`** -- the safe, recommended entry point. It verifies the
  process is single-threaded, then daemonizes. No `unsafe` needed. Available on
  **Linux, macOS, FreeBSD, NetBSD, and OpenBSD** (it reads the kernel's thread
  count: `/proc/self/status` on Linux, `proc_pidinfo` on macOS, `sysctl` on the
  BSDs). On any other target it is a `#[deprecated]` stub that never daemonizes.
- **`unsafe { daemonize_unchecked(&config) }`** -- the escape hatch, available on
  all Unix platforms. It skips the thread-count check, so *you* must guarantee
  the process is single-threaded (see
  [Safety](#safety-the-single-threaded-rule)).

To stay portable across *every* Unix, gate the call so the deprecated stub is
never built on targets that lack a thread-count source:

```rust
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd",
          target_os = "netbsd", target_os = "openbsd"))]
let mut ctx = blivet::daemonize(&config)?;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd",
              target_os = "netbsd", target_os = "openbsd")))]
// SAFETY: no threads spawned before this point.
let mut ctx = unsafe { blivet::daemonize_unchecked(&config)? };
```

### Split-phase privilege dropping

When your daemon needs to perform privileged operations (like binding to
port 80, calling `chroot`, or setting resource limits) before dropping to
an unprivileged user:

```rust
use blivet::{DaemonConfig, daemonize};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = DaemonConfig::new();
    config
        .pidfile("/var/run/myapp.pid")
        .user("www-data")
        .group("www-data");

    let mut ctx = daemonize(&config)?;

    // Still running as root here -- bind privileged port, chroot, set rlimits, etc.
    // let listener = TcpListener::bind("0.0.0.0:80")?;

    // Transfer file ownership, then drop privileges
    ctx.chown_paths()?;
    ctx.drop_privileges()?;

    ctx.notify_parent()?;
    // Now running as www-data...
    Ok(())
}
```

### Foreground mode

For systemd, containers, or debugging, use foreground mode to skip forking
while still applying all other daemon setup (umask, chdir, signal reset, etc.).
Stdout and stderr are left inherited (not redirected to `/dev/null`) unless
explicitly configured with `.stdout()`/`.stderr()`:

```rust
let mut config = DaemonConfig::new();
config
    .foreground(true)
    .close_fds(false);  // keep supervisor-passed fds
```

## Safety: the single-threaded rule

Forking a multithreaded process is unsound: mutexes held by other threads stay
locked forever in the child, deadlocking it. So daemonization must happen
*before* you spawn any threads or start an async runtime.

`daemonize()` enforces this for you -- it reads the kernel thread count and
panics if it isn't exactly 1, then forks. `daemonize_unchecked()` is `unsafe`
precisely because it skips that check and trusts the caller. Either way, the
single-threaded requirement applies **only at fork time**:

```text
[single-threaded required]
  daemonize()           <- forks here
  chown_paths()         <- still single-threaded
  drop_privileges()     <- still single-threaded (calls setenv, not thread-safe)
  notify_parent()
[now safe to spawn threads / start tokio / accept connections]
```

Start threads, an async runtime, or a thread-per-connection accept loop only
*after* `notify_parent()`.

## API overview

Full reference is on [docs.rs](https://docs.rs/blivet); this is the shape of it.

### `DaemonConfig`

A builder of infallible `&mut self` setters; validation is deferred to
`validate()` (which `daemonize()` runs for you). Common settings: `pidfile`,
`lockfile`, `stdout`/`stderr` (+ `append`), `chdir`, `umask`, `user`/`group`,
`foreground`, `close_fds`, `cleanup_on_drop`, and `env`. Defaults worth knowing:
working directory is `/`, stdout/stderr go to `/dev/null`, `close_fds` and
`cleanup_on_drop` are `true`. See the
[`DaemonConfig` docs](https://docs.rs/blivet/latest/blivet/struct.DaemonConfig.html)
for every method.

### The `daemonize` call

`daemonize(&config) -> Result<DaemonContext, DaemonizeError>` performs the full
sequence: pipe, double-fork, setsid, umask, chdir, `/dev/null` redirect,
lockfile, pidfile, signal reset, signal mask clear, env vars, output redirect,
fd close. It returns a `DaemonContext` in the grandchild (or the current process
in foreground mode); the original parent blocks on the notification pipe.

User/group switching is **not** performed during this call -- use
`DaemonContext::drop_privileges()` after any privileged work.

### `DaemonContext`

Returned by a successful `daemonize()`; owns the lockfile, notification pipe,
and the state needed for privilege operations. The methods you'll use most:

- `notify_parent()` -- signal readiness so the parent exits 0. **Dropping the
  context without calling it makes the parent exit non-zero.**
- `chown_paths()` / `drop_privileges()` -- transfer file ownership, then switch
  user/group (`initgroups` + `setgid` + `setuid`).
- `cleanup()` and `cleanup_on_term_signals()` -- remove the pidfile (see
  [pidfile cleanup](#pidfile-cleanup-on-signals)).
- `report_error(_msg)` / `notify_parent_or_report()` -- report a failure to the
  parent and `_exit` (see [recipes](#reporting-your-own-failures)).

See the
[`DaemonContext` docs](https://docs.rs/blivet/latest/blivet/struct.DaemonContext.html)
for the full set.

### Errors & exit codes

`DaemonizeError` has sixteen variants covering validation, fork, setsid, lock,
permission, chown, exec, and parent-notify failures, plus a caller-supplied
`Application` variant. Each maps to a `sysexits.h` exit code via `exit_code()`,
so failures reach the shell with a meaningful status:

| Variant            | Exit code   | Meaning                                  |
| ------------------ | ----------- | ---------------------------------------- |
| `ValidationError`  | 64          | Bad config (paths, env keys, overlaps)   |
| `ProgramNotFound`  | 66          | CLI: program missing or not executable   |
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
| `ExecFailed`       | 71          | CLI: `exec` of target program failed     |
| `NotifyFailed`     | 71          | Writing readiness byte to parent failed  |
| `PrivilegesNotDropped` | 70      | user/group set but `drop_privileges()` never called |
| `Application`      | caller's    | App-level failure you report yourself    |

## Recipes

### Pidfile cleanup on signals

When `cleanup_on_drop` is `true` (the default), the pidfile is removed when
`DaemonContext` is dropped. However, **`Drop` does not run when the process is
killed by a signal** (`SIGTERM`, `SIGKILL`, etc.) — which is how most daemons
are stopped, so the pidfile would be left behind.

The simplest fix is the built-in `cleanup_on_term_signals()`, which installs
async-signal-safe handlers that remove the pidfile on `SIGINT`/`SIGTERM` and
then re-raise so the process still terminates normally:

```rust
use blivet::{DaemonConfig, daemonize};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = DaemonConfig::new();
    config.pidfile("/var/run/myapp.pid");
    let mut ctx = daemonize(&config)?;

    // Remove the pidfile on SIGINT/SIGTERM (pass custom signal numbers to
    // `cleanup_on_signals(&[...])` instead, if needed).
    ctx.cleanup_on_term_signals()?;

    ctx.notify_parent()?;
    // ... daemon work ...
    Ok(())
}
```

> **Note:** this is library-only. The `daemonize` **CLI** cannot do this: it
> `exec`s the target program, and `exec` resets all custom signal handlers to
> their default disposition. A program launched via the CLI must clean up its
> own pidfile.

If you already run a signal loop (e.g. to drive a graceful shutdown), you can
instead clear the flag yourself and call `cleanup()` / let `ctx` drop. The
example below uses the [`signal_hook`](https://crates.io/crates/signal-hook)
crate for that; `blivet` does not re-export it, so add it yourself:

```sh
cargo add signal_hook
```

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use blivet::{DaemonConfig, daemonize};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = DaemonConfig::new();
    let mut ctx = daemonize(&config)?;
    ctx.notify_parent()?;

    // Set a flag on SIGTERM/SIGINT so the main loop exits cleanly
    let running = Arc::new(AtomicBool::new(true));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&running))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&running))?;

    while running.load(Ordering::Relaxed) {
        // ... daemon work ...
    }

    ctx.cleanup(); // or just let ctx drop
    Ok(())
}
```

### Reporting your own failures

If startup work in the privileged init window fails (a socket bind, a database
connect), report it to the parent with a `sysexits.h` code of your choosing via
`report_error_msg` — no need to construct a `DaemonizeError` by hand:

```rust
let listener = match TcpListener::bind("0.0.0.0:80") {
    Ok(l) => l,
    // 71 == EX_OSERR; the parent prints the message and exits with this code.
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
    // ... application init ...
    // notify_parent() returns DaemonizeError (NotifyFailed, exit 71), so `?`
    // keeps a single error type and preserves the exit code.
    ctx.notify_parent()?;
    Ok(())
}
```

## Minimum supported Rust version

1.85

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
