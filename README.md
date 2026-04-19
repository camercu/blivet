# blivet

[![CI](https://github.com/camercu/blivet/actions/workflows/ci.yml/badge.svg)](https://github.com/camercu/blivet/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/blivet.svg)](https://crates.io/crates/blivet)
[![docs.rs](https://docs.rs/blivet/badge.svg)](https://docs.rs/blivet)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-blue)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)](LICENSE-MIT)

A correct, minimal Unix daemon library and CLI for Rust.

A [blivet](https://en.wikipedia.org/wiki/Impossible_trident) is the "impossible
fork" optical illusion, also known as the devil's tuning fork. Daemons are
created by forking — and this crate performs the impossible double-fork to do it
correctly.

`blivet` implements the full double-fork daemonization sequence with a
parent-notification pipe, so your process detaches cleanly and the calling shell
(or init system) knows exactly when the daemon is ready -- or why it failed.
Errors that happen after forking are reported back to the parent with
`sysexits.h` codes instead of vanishing into a void.

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
- **Unsafe contained.** `#![deny(unsafe_code)]` at the crate root. All unsafe
  lives in a single module (`unsafe_ops`), with safe wrappers for everything
  else.
- **Library and CLI.** Use it as a Rust library with a builder API, or as a
  standalone `daemonize` binary (installed by `cargo install blivet`) that
  wraps any program.

## Install

```sh
cargo install blivet
```

Or add the library to your project:

```sh
cargo add blivet
```

## CLI Quickstart

Daemonize any program:

```sh
# Basic usage
daemonize -- /usr/bin/my-server --port 8080

# With pidfile and log redirection (stderr mirrors stdout by default)
daemonize \
  -p /var/run/myapp.pid \
  -o /var/log/myapp.log \
  -c /var/lib/myapp \
  -- /usr/bin/my-server

# Split stdout/stderr using .stdout/.stderr or .out/.err extensions (auto-derived)
daemonize \
  -p /var/run/myapp.pid \
  -o /var/log/myapp.stdout \
  -- /usr/bin/my-server  # stderr goes to /var/log/myapp.stderr

# Separate lockfile (overrides the pidfile default)
daemonize \
  -p /var/run/myapp.pid \
  -l /var/run/myapp.lock \
  -- /usr/bin/my-server

# Run as a different user and group (requires root)
daemonize -u www-data -g www-data -- /usr/bin/my-server

# Run in foreground with supervisor-passed fds kept open
daemonize --foreground --no-close-fds -p /var/run/myapp.pid -- /usr/bin/my-server

# Set environment variables
daemonize -E RUST_LOG=info -E PORT=8080 -- /usr/bin/my-server
```

The parent process blocks until the daemon successfully calls `exec`, then
exits 0. If anything fails (lockfile conflict, permission denied, exec error),
the parent prints the error to stderr and exits with a `sysexits.h` code.

When `-u`/`-g` are specified, the CLI transfers ownership of the pidfile,
lockfile, and log files to the target user/group before dropping privileges,
so the daemon can continue to write to them after the switch.

### CLI flags

| Flag | Long                | Description                                                          |
| ---- | ------------------- | -------------------------------------------------------------------- |
| `-p` | `--pidfile PATH`    | Write daemon PID to file                                             |
| `-l` | `--lock PATH`       | Exclusive lockfile (default: pidfile path, if set)                   |
| `-c` | `--chdir PATH`      | Working directory (default: `/`)                                     |
| `-m` | `--umask MODE`      | Process umask in octal (e.g. `022`)                                  |
| `-o` | `--stdout PATH`     | Redirect stdout to file (also sets stderr if `-e` is not given)      |
| `-e` | `--stderr PATH`     | Redirect stderr to file (default: stdout path; `.stdout`→`.stderr`, `.out`→`.err`) |
| `-a` | `--append`          | Append to stdout/stderr files instead of truncating                  |
| `-u` | `--user NAME\|UID`  | Switch to user after daemonizing (requires root)                     |
| `-g` | `--group NAME\|GID` | Switch to group after daemonizing (requires root)                    |
| `-f` | `--foreground`      | Stay in foreground (no fork/setsid); consider `--no-close-fds`       |
|      | `--no-close-fds`    | Keep inherited fds open (useful with `-f` for supervisor-passed fds) |
| `-E` | `--env NAME=VAL`    | Set environment variable (repeatable)                                |
| `-v` | `--verbose`         | Print diagnostic info before daemonizing                             |

## Library quickstart

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

    // SAFETY: must be called before spawning any threads.
    // daemonize() validates the config internally before forking.
    let mut ctx = unsafe { daemonize(&config)? };

    // Application initialization goes here (open sockets, load config, etc.)

    // Signal the parent that we're ready.
    ctx.notify_parent()?;

    // Daemon continues running...
    Ok(())
}
```

### Split-phase privilege dropping

When your daemon needs to perform privileged operations (like binding to
port 80) before dropping to an unprivileged user:

```rust
use blivet::{DaemonConfig, daemonize};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = DaemonConfig::new();
    config
        .pidfile("/var/run/myapp.pid")
        .user("www-data")
        .group("www-data");

    // SAFETY: must be called before spawning any threads.
    let mut ctx = unsafe { daemonize(&config)? };

    // Still running as root here -- bind privileged port
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
while still applying all other daemon setup (umask, chdir, signal reset, etc.):

```rust
let mut config = DaemonConfig::new();
config
    .foreground(true)
    .close_fds(false);  // keep supervisor-passed fds
```

On Linux, `daemonize_checked` provides a safe wrapper that verifies the process
is single-threaded (via `/proc/self/status`) before forking:

```rust
use blivet::{DaemonConfig, daemonize_checked};

let config = DaemonConfig::new();
let mut ctx = daemonize_checked(&config)?; // panics if threads > 1
ctx.notify_parent()?;
```

## API overview

### `DaemonConfig`

Builder for daemonization settings. All methods are infallible setters;
validation is deferred to `validate()`.

| Method             | Default | Description                                           |
| ------------------ | ------- | ----------------------------------------------------- |
| `pidfile(path)`    | None    | Write PID to file                                     |
| `lockfile(path)`   | None    | Exclusive flock-based lockfile                        |
| `chdir(path)`      | `/`     | Working directory                                     |
| `umask(mode)`      | `0`     | Process umask                                         |
| `stdout(path)`     | None    | Redirect stdout (stays `/dev/null` if unset)          |
| `stderr(path)`     | None    | Redirect stderr (stays `/dev/null` if unset)          |
| `append(bool)`     | `false` | Append vs truncate output files                       |
| `user(name)`       | None    | Switch user -- name or numeric UID (requires root)    |
| `group(name)`      | None    | Switch group -- name or numeric GID (requires root)   |
| `foreground(bool)` | `false` | Skip fork/setsid (for systemd, containers, debugging) |
| `close_fds(bool)`  | `true`  | Close inherited fds 3+                                |
| `env(key, val)`    | None    | Set env var (accumulates, last-write-wins)            |
| `validate()`       | --      | Check paths, permissions, overlaps before forking     |

### `daemonize(&config) -> Result<DaemonContext, DaemonizeError>`

Performs the daemonization sequence: pipe, double-fork, setsid, umask, chdir,
`/dev/null` redirect, lockfile, pidfile, signal reset, signal mask clear, env
vars, output redirect, fd close. Returns a `DaemonContext` in the grandchild
(or the current process in foreground mode). The original parent blocks on the
notification pipe.

User/group switching is **not** performed during this call. Use
`DaemonContext::drop_privileges()` after doing any privileged work.

### `DaemonContext`

Returned by a successful `daemonize()` call. Holds the lockfile, notification
pipe, and config state needed for privilege operations.

| Method              | Description                                                  |
| ------------------- | ------------------------------------------------------------ |
| `chown_paths()`     | Transfer pidfile/lockfile/log ownership to target user/group |
| `drop_privileges()` | Switch user/group (`initgroups` + `setgid` + `setuid`)       |
| `notify_parent()`   | Signal readiness -- parent exits 0                           |
| `report_error(err)` | Report error to parent and `_exit`                           |
| `lockfile_fd()`     | Borrow the lockfile fd (if configured)                       |

Dropping without calling `notify_parent()` causes the parent to exit non-zero.

### `DaemonizeError`

Fourteen variants covering validation, fork, setsid, lock, permission, chown,
and exec failures. Each maps to a `sysexits.h` exit code via `exit_code()`.

| Variant            | Exit code | Meaning                                  |
| ------------------ | --------- | ---------------------------------------- |
| `ValidationError`  | 64        | Bad config (paths, env keys, overlaps)   |
| `ProgramNotFound`  | 66        | CLI: program missing or not executable   |
| `UserNotFound`     | 67        | User doesn't exist                       |
| `GroupNotFound`    | 67        | Group doesn't exist                      |
| `LockConflict`     | 69        | Lockfile held by another process         |
| `LockfileError`    | 73        | Can't open lockfile                      |
| `PidfileError`     | 73        | Can't write pidfile                      |
| `OutputFileError`  | 73        | Can't open/redirect output file          |
| `ChownError`       | 73        | Can't chown pidfile/lockfile/output file |
| `ForkFailed`       | 71        | `fork()` error                           |
| `SetsidFailed`     | 71        | `setsid()` error                         |
| `ChdirFailed`      | 71        | `chdir()` error                          |
| `PermissionDenied` | 77        | Not root, or setuid/setgid failed        |
| `ExecFailed`       | 71        | CLI: `exec` of target program failed     |

## Minimum supported Rust version

1.85

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT License](LICENSE-MIT) at your option.
