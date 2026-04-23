# blivet Specification

Rust library and CLI tool for daemonizing processes. Mirrors Debian's
daemonize(1) with improvements: double-fork is mandatory (not delegated
to `daemon(3)`), signal dispositions and mask are reset, and a
notification pipe lets the parent wait for daemon readiness. Privilege
dropping is split-phase: `daemonize()` returns a context while still
privileged, and the caller explicitly calls `drop_privileges()` when
ready.

The name comes from the [blivet](https://en.wikipedia.org/wiki/Impossible_trident),
the "impossible fork" optical illusion (a.k.a. the devil's tuning fork).
Daemons are created by forking — and this crate performs the impossible
double-fork to do it correctly. The CLI binary is named `daemonize`.

Latest stable Rust, edition 2021. Crate name is `blivet`. License:
`MIT OR Apache-2.0`; include both `LICENSE-MIT` and `LICENSE-APACHE`
files.

> The crate exposes `nix::sys::stat::Mode` in the public API (the
> `umask()` builder). Since nix is pre-1.0 (0.29), this crate must
> remain pre-1.0 (0.x) until nix stabilizes or `Mode` is removed from
> the public surface. A newtype wrapper adds friction for users who
> already depend on nix and provides no safety benefit.

Dependencies: nix (0.31, features: `fs`, `signal`, `process`,
`user`, `resource`), clap, thiserror, libc. Dev: tempfile,
serial_test, relentless.

## Dependency policy

Prefer nix crate safe wrappers over direct libc calls. Use libc only
when nix does not provide a safe alternative. Known exceptions:
`libc::SIGRTMAX()`, `libc::SIGRTMIN()`, and `libc::sigaction()` for
real-time signal iteration.

> nix's `Signal` type covers only the 31 standard POSIX signals and
> cannot represent real-time signal numbers.

## Unsafe boundary

The crate root uses `#![deny(unsafe_code)]`. All `unsafe` blocks are
confined to a single internal `unsafe_ops` module marked with
`#![allow(unsafe_code)]`. This module re-exports safe `pub(crate)`
wrapper functions. An auditor can read one module to verify all unsafe
code.

Contents of `unsafe_ops`:

- `RealForker` implementation (calls `nix::unistd::fork()`, which is
  `unsafe` since nix 0.24).
- Real-time signal reset loop (`libc::sigaction()`).
- `libc::SIGRTMAX()` / `libc::SIGRTMIN()` wrappers.

Nothing else in the crate contains `unsafe` blocks. The public
`daemonize()` function is `unsafe fn` (see below); its unsafety is a
caller contract, not an `unsafe` block in the implementation.

---

## Public API

### DaemonConfig

`DaemonConfig` is a plain data struct with `&mut self` builder methods.
It derives `Default`, `Debug`, `Clone`, `Eq`, and `PartialEq`.
`DaemonConfig::new()` returns the same value as `Default::default()`.

All fields are private; callers use builder methods.

| Field        | Type                    | Default              | Builder method       | Builder input type         | Semantics   |
| ------------ | ----------------------- | -------------------- | -------------------- | -------------------------- | ----------- |
| `pidfile`    | `Option<PathBuf>`       | `None`               | `.pidfile(path)`     | `impl Into<PathBuf>`       | Setter      |
| `chdir`      | `PathBuf`               | `PathBuf::from("/")` | `.chdir(path)`       | `impl Into<PathBuf>`       | Setter      |
| `umask`      | `nix::sys::stat::Mode`  | `Mode::empty()` (0)  | `.umask(mode)`       | `nix::sys::stat::Mode`     | Setter      |
| `stdout`     | `Option<PathBuf>`       | `None`               | `.stdout(path)`      | `impl Into<PathBuf>`       | Setter      |
| `stderr`     | `Option<PathBuf>`       | `None`               | `.stderr(path)`      | `impl Into<PathBuf>`       | Setter      |
| `append`     | `bool`                  | `false`              | `.append(bool)`      | `bool`                     | Setter      |
| `lockfile`   | `Option<PathBuf>`       | `None`               | `.lockfile(path)`    | `impl Into<PathBuf>`       | Setter      |
| `user`       | `Option<String>`        | `None`               | `.user(name)`        | `impl Into<String>`        | Setter      |
| `group`      | `Option<String>`        | `None`               | `.group(name)`       | `impl Into<String>`        | Setter      |
| `foreground` | `bool`                  | `false`              | `.foreground(bool)`  | `bool`                     | Setter      |
| `close_fds`  | `bool`                  | `true`               | `.close_fds(bool)`   | `bool`                     | Setter      |
| `cleanup_on_drop` | `bool`             | `true`               | `.cleanup_on_drop(bool)` | `bool`                 | Setter      |
| `env`        | `Vec<(String, String)>` | `vec![]`             | `.env(key, value)`   | `impl Into<String>` (both) | Accumulator |

All builder methods except `.env()` are setters: each call replaces the
previous value. `.env()` is an accumulator: each call pushes a new
entry in insertion order. Last-write-wins for duplicate keys is achieved
at application time (step 11) via sequential `setenv` calls. Empty
string values are legal (POSIX `setenv` semantics). There is no
`.clear_env()` method; construct a new `DaemonConfig` to start over.

All builder methods are infallible (`&mut Self`, no `Result`). All
input validation is centralized in `validate()`.

```rust
let mut config = DaemonConfig::new();
config.pidfile("/var/run/foo.pid").chdir("/tmp");

config.validate()?;                     // optional early check

let mut ctx = unsafe { daemonize(&config)? };
// ... application initialization (still privileged if run as root) ...
ctx.notify_parent()?;
// daemon process continues here
```

**Split-phase privilege dropping** (when user/group switching is needed):

```rust
let mut config = DaemonConfig::new();
config.pidfile("/var/run/foo.pid").user("nobody").group("nogroup");

let mut ctx = unsafe { daemonize(&config)? };
// privileged work here (e.g., bind port 80)
ctx.chown_paths()?;                     // chown pidfile/lockfile/logs to target user
ctx.drop_privileges()?;                 // setgid + setuid
ctx.notify_parent()?;
```

### DaemonContext

`daemonize()` returns `Result<DaemonContext, DaemonizeError>`.
`DaemonContext` is `#[non_exhaustive]` with all private fields. It
derives `Debug` (Flock formatted as present/absent). Do not derive
`Clone` or `PartialEq`.

| Field           | Type               | Semantics                                          |
| --------------- | ------------------ | -------------------------------------------------- |
| `lockfile`      | `Option<Flock>`    | Owned lock (`nix::fcntl`); drop releases.          |
| `notify_pipe`   | `Option<OwnedFd>`  | Write end of notification pipe; see below.         |
| `pidfile`       | `Option<PathBuf>`  | Cloned from config; used by `chown_paths()` and `cleanup()`. |
| `lockfile_path` | `Option<PathBuf>`  | Cloned from config; used by `chown_paths()`.       |
| `stdout`        | `Option<PathBuf>`  | Cloned from config; used by `chown_paths()`.       |
| `stderr`        | `Option<PathBuf>`  | Cloned from config; used by `chown_paths()`.       |
| `user`          | `Option<String>`   | Cloned from config; used by `drop_privileges()`.   |
| `group`         | `Option<String>`   | Cloned from config; used by `drop_privileges()`.   |
| `cleanup_on_drop` | `bool`           | From config; controls whether `cleanup()` runs on drop. |
| `cleaned_up`    | `bool`             | Internal flag; prevents double cleanup.            |

**Accessors:**

`lockfile_fd()` returns `Option<BorrowedFd<'_>>` (lifetime tied to the
context). Returns `None` when no lockfile was configured.

`set_cleanup_on_drop(bool)` overrides the config-level
`cleanup_on_drop` setting at runtime.

**Cleanup:**

`cleanup(&mut self)` removes the pidfile from disk (best-effort).
Standalone lockfiles are left on disk; the flock is released when
`DaemonContext` drops. Errors are silently ignored. Idempotent via
an internal `cleaned_up` flag. Note that `Drop` does not run when the
process is killed by a signal — callers must install a signal handler
to ensure `cleanup()` runs (either explicitly or by dropping
`DaemonContext` at main-loop exit).

**Privilege methods:**

`chown_paths(&mut self) -> Result<(), DaemonizeError>` changes
ownership of pidfile, lockfile, stdout, and stderr files to the
resolved target user/group. Skips files that are not configured. Must
be called while still privileged (before `drop_privileges()`). No-op
if neither user nor group is configured. User/group resolution uses
the same string-parsing strategy as `drop_privileges()` (see below).

`drop_privileges(&mut self) -> Result<(), DaemonizeError>` performs
user/group switching. Resolution: if the string parses as a `u32`,
treat it as a numeric UID/GID; otherwise resolve via `getpwnam()`/
`getgrnam()`. Four combinations:

- Neither user nor group: no-op.
- User only: resolve user via `getpwnam()`, `initgroups(username,
  primary_gid)`, `setgid(primary_gid)`, `setuid(uid)`.
- Both user and group: resolve user via `getpwnam()`, resolve group
  via `getgrnam()`, `initgroups(username, user_primary_gid)`,
  `setgid(group_gid)`, `setuid(uid)`.
- Group only: resolve group via `getgrnam()`, `setgid(group_gid)`.

After user switching, sets `USER`, `HOME`, `LOGNAME` environment
variables. These unconditionally overwrite any `.env()` values from
the daemonization sequence.

**Notification methods:**

`notify_parent(&mut self) -> Result<(), io::Error>` writes a success
byte (`0x00`) to the notification pipe and closes it. The parent reads
this byte and exits 0. After this call, `notify_pipe` is `None`. This
is the API for library consumers to signal readiness.

`report_error(&mut self, err: &DaemonizeError) -> !` writes the error
protocol to the notification pipe (exit code byte + Display message),
closes it, then calls `_exit()` with the mapped exit code. This is used
by the CLI to report post-daemonization failures (e.g., exec failure).

**Drop behavior:** if `notify_pipe` is still `Some` when `DaemonContext`
is dropped, the `Drop` impl writes exit code `1` followed by the
message "daemon exited without signaling readiness" to the pipe, then
closes it. The parent reads this and exits 1. If `cleanup_on_drop` is
`true` (the default), `Drop` then calls `cleanup()` to remove the
pidfile.

> Exit code `1` (not a `sysexits.h` value) is used for the Drop case
> to distinguish it from categorized `DaemonizeError` failures.

> The notification pipe lets the original process (or init system)
> distinguish "daemon started successfully" from "daemon died during
> setup." Without it, the parent exits 0 before setup completes, and
> post-fork failures are invisible. The Drop behavior catches library
> consumers who forget to call `notify_parent()`.

### DaemonizeError

One enum covering validation and runtime errors. Derives `Debug` and
`thiserror::Error` (providing `Display` and `std::error::Error`). Must
be `Send + Sync` (compile-time assertion in tests). Do not derive
`Clone` or `PartialEq`; use `matches!()` in tests. `Display` messages
are lowercase with no trailing punctuation.

| Variant            | Condition                                               |
| ------------------ | ------------------------------------------------------- |
| `ValidationError`  | Bad path, bad env key, path overlap, other config error |
| `ProgramNotFound`  | CLI-only: program path missing or not executable        |
| `UserNotFound`     | User does not exist at runtime during user switching    |
| `GroupNotFound`    | Group does not exist at runtime during group switching  |
| `LockConflict`     | flock already held by another process                   |
| `LockfileError`    | Lockfile cannot be opened                               |
| `ForkFailed`       | `fork()` returns an error                               |
| `SetsidFailed`     | `setsid()` returns an error                             |
| `ChdirFailed`      | `chdir()` fails at runtime after fork                   |
| `PermissionDenied` | Non-root caller with user/group switch, or setuid/setgid/chown fail |
| `PidfileError`     | Pidfile cannot be written                               |
| `OutputFileError`  | stdout/stderr file cannot be opened/dup2'd              |
| `ChownError`       | chown of pidfile/lockfile/output file failed             |
| `ExecFailed`       | CLI-only: exec of target program failed                 |

`ProgramNotFound` and `ExecFailed` are produced only by the CLI, never
by the library. They are in the shared enum so the CLI can use the
library's error type and notification pipe protocol uniformly.

`exit_code(&self) -> u8` returns the `sysexits.h` code for the variant
(see CLI exit code table).

> Placing exit code mapping on the error type (rather than keeping it
> as a CLI-only concern) is motivated by the notification pipe: the
> library's parent-side pipe reader must convert errors to exit codes
> for the parent process. The mapping is therefore a library concern.

> `PermissionDenied` covers both validation-time (non-root caller) and
> runtime (`initgroups`/`setgid`/`setuid`) failures. Callers
> distinguish by context: validation returns before any fork; runtime
> after. A future iteration can split the runtime case into a new
> variant without breaking validation semantics.

### daemonize()

```rust
/// # Safety
///
/// No other threads may be running when this function is called.
/// Forking a multithreaded process leaves mutexes held by other
/// threads permanently locked in the child, causing deadlocks or
/// undefined behavior. Call before spawning threads, async runtimes,
/// or libraries with background threads.
pub unsafe fn daemonize(
    config: &DaemonConfig,
) -> Result<DaemonContext, DaemonizeError>
```

Takes `&DaemonConfig`, calls `validate()` internally, then performs the
daemonization sequence. Returns `Ok(DaemonContext)` in the grandchild
on success (or in the current process when `foreground` is true).

In foreground mode, the function skips both forks, `setsid`, and the
notification pipe. All other steps (umask, chdir, redirect, signal
reset, env vars, lockfile, pidfile) still execute. The returned
`DaemonContext` has `notify_pipe: None`, making `notify_parent()` a
no-op.

**Error paths:**

- Pre-fork errors (validation failure, pipe creation failure, first
  fork failure) return `Err` directly to the original caller. No
  notification pipe is involved because the caller _is_ the parent.
- Post-fork errors (steps 2–14) are written to the notification pipe
  per the notification protocol. The grandchild calls `_exit()`. The
  parent reads the error, prints it to its stderr, and calls `_exit()`
  with the mapped exit code. The grandchild's `_exit()` code uses the
  mapped code for consistency in process accounting, but is not
  observable — the parent's exit code is authoritative.

The library does not accept a program path or call exec.

### daemonize_checked()

```rust
#[cfg(target_os = "linux")]
pub fn daemonize_checked(
    config: &DaemonConfig,
) -> Result<DaemonContext, DaemonizeError>
```

Safe wrapper. Reads `/proc/self/status`, parses the `Threads:` line.
If the count exceeds 1, panics with a message naming the problem and
the fix. If `/proc/self/status` cannot be read or parsed, also panics.
Then calls `unsafe { daemonize(config) }`.

> On Linux with a single thread, the observation "thread count is 1"
> is stable: incrementing the count requires an existing thread to call
> `pthread_create`, but no other thread exists. The check-then-fork
> sequence has no race.

### validate()

Public method on `DaemonConfig`: `fn validate(&self) -> Result<(),
DaemonizeError>`. Calling before `daemonize()` is optional;
`daemonize()` always re-validates.

---

## Validation

Checks paths, permissions, and config values against the real
filesystem using the **current effective UID** at the time of the call.

> When a user switch is configured, validation runs as root but the
> daemon operates as the target user. Post-user-switch permission
> failures are runtime errors. TOCTOU is inherent; `daemonize()` still
> handles runtime failures.

### Path rules

Pidfile, stdout, stderr, and lockfile paths must be absolute when
configured. Chdir path must be absolute, must exist, and must be a
directory. Pidfile path must not be a directory. Parent directories of
all configured paths must be writable (checked against current euid).

### Path comparison

All overlap and sameness checks use `std::fs::canonicalize()` on both
operands. If either path does not exist, fall back to byte-equal
`PathBuf` comparison.

> Overlap detection is best-effort for not-yet-created files. The
> actual failure mode (fd interference) would produce a clear runtime
> error.

### Path overlap rules

Lockfile and pidfile may be the same path (see step 8). Neither may
equal a configured stdout or stderr path. The overlap check applies
only when both paths in a pair are `Some`.

> The lockfile fd is long-lived; stdout/stderr fds are redirected to
> independent file descriptions. Overlap would cause one open to
> interfere with the other.

### User/group validation

`validate()` checks only that euid is 0 when either a user or group is
configured. It does not call `getpwnam()` or `getgrnam()`.

> User/group lookups can trigger NSS/LDAP/NIS network calls with
> unpredictable latency and failure modes. Resolution is a runtime
> concern handled by `drop_privileges()`. Consumers who want early
> validation should call `getpwnam()`/`getgrnam()` themselves.

### Environment key validation

Keys must be non-empty and contain no `=`. Values are unrestricted.

> This check is in `validate()`, not `.env()`, because all builder
> methods are infallible. Centralizing validation keeps the API
> consistent.

---

## Daemonization sequence

After `validate()` passes, `daemonize()` performs these steps in this
exact order. The ordering is load-bearing.

### Foreground mode

When `foreground` is true, steps 1–3 (pipe creation, both forks,
setsid) are skipped entirely. Execution continues from step 4 in the
current process. The returned `DaemonContext` has `notify_pipe: None`.

### Steps

1. **Create notification pipe, first fork.** *(Skipped in foreground
   mode.)* Create a pipe with `O_CLOEXEC` on both ends (`pipe_rd`,
   `pipe_wr`). Fork. If fork fails, close both pipe ends and return
   `Err(ForkFailed)` to the caller — no pipe protocol is involved.
   On success: parent closes `pipe_wr` and enters the parent-side
   pipe reader (see notification protocol); child closes `pipe_rd`
   and continues with `pipe_wr`.
2. **setsid.** *(Skipped in foreground mode.)* Failure: write
   `SetsidFailed` to pipe, `_exit()`.
3. **Second fork.** *(Skipped in foreground mode.)* If fork fails:
   write `ForkFailed` to pipe, `_exit()`. On success: intermediate
   child calls `_exit(0)`; grandchild continues with `pipe_wr`.
4. **Set umask.**
5. **chdir.** Failure: write `ChdirFailed` to pipe, `_exit()`.
6. **Redirect stdin, stdout, stderr to `/dev/null`.** See
   /dev/null redirect policy.
7. **Open and lock lockfile** (if configured). Open with
   `O_WRONLY | O_CREAT | O_CLOEXEC`, mode 0644, then
   `flock(LOCK_EX | LOCK_NB)`. Open failure: `LockfileError`.
   Flock failure: `LockConflict`. Fd is retained through step 13.
8. **Write pidfile** (if configured). See pidfile mechanics.
9. **Reset signal dispositions.** See signal reset.
10. **Clear signal mask** via `sigprocmask(SIG_SETMASK, empty_set)`.
11. **Set environment variables.** Sequential `setenv` in insertion
    order; last-write-wins for duplicate keys. Inherited environment
    is preserved.
12. **Redirect stdout/stderr to configured files** (if configured).
    See output file redirect.
13. **Close inherited fds** (if `close_fds` is true). Iterate
    3..`rlim_cur`, skipping lockfile fd and notification pipe fd.
    See fd closing.
14. **Return `DaemonContext`** owning lockfile `Option<Flock>`,
    `pipe_wr` as `Option<OwnedFd>`, and cloned path/user/group
    fields from config.

> User/group switching (formerly step 12) is no longer performed by
> `daemonize()`. It is the caller's responsibility to call
> `ctx.drop_privileges()` after daemonization. This split-phase
> design gives the caller a privileged window between `daemonize()`
> and `drop_privileges()` for operations like binding privileged
> ports. See `DaemonContext::drop_privileges()` above.

### Post-fork error policy

Post-fork errors in steps 2–13 that have a named `DaemonizeError`
variant are written to the notification pipe per the error protocol
and the process calls `_exit()` with the mapped exit code. Unspecified
syscall failures (e.g., `sigprocmask`, `setenv`) panic with a
descriptive message. Individual `close()` errors in step 13 are
silently ignored.

> Panics for unspecified failures indicate a broken OS environment
> where no reasonable recovery is possible, consistent with the
> `/dev/null` panic rationale. The `_exit()` code is not observable
> (the parent reads its exit code from the pipe), but uses the mapped
> code for consistency in process accounting.

### Notification pipe protocol

The parent-side reader (in step 1's parent branch) reads from the pipe
and takes one of three actions:

- **Success byte (0x00):** library consumer called `notify_parent()`.
  Parent calls `_exit(0)`.
- **Error byte (nonzero):** value is the `exit_code()` of the error.
  Remaining bytes are the `Display` message as UTF-8. Parent prints
  the message to stderr and calls `_exit()` with the code.
- **EOF (no bytes read):** the pipe was closed without any write. This
  means `exec` succeeded and `O_CLOEXEC` closed the write end. Parent
  calls `_exit(0)`.

> EOF-means-success is designed for the CLI exec path: `execvp`
> replaces the process image, and `O_CLOEXEC` on the pipe write end
> closes it automatically, producing EOF. For library consumers who
> never exec, `notify_parent()` writes the explicit `0x00` byte. If a
> library consumer drops `DaemonContext` without calling
> `notify_parent()`, the `Drop` impl writes a failure byte before
> closing, ensuring the parent exits non-zero. The only case where
> EOF produces a false positive is a hard crash (e.g., `SIGKILL`) that
> prevents `Drop` from running — this is an inherent limitation, and
> the same as every other daemonization tool.

### /dev/null redirect policy

Step 6 redirects all three standard fds to `/dev/null`. For each fd:
open `/dev/null`, `dup2` to the target fd, close the source fd. If the
source fd already equals the target, skip `dup2` and close. Failure to
open `/dev/null` or `dup2` to it panics.

> A system without `/dev/null` is fundamentally broken. No recovery is
> possible.

### Output file redirect

Step 12 opens configured stdout/stderr files and redirects the
corresponding fds. Since user switching is now the caller's
responsibility (via `drop_privileges()`), files are created as the
current user (typically root). Use `chown_paths()` before
`drop_privileges()` to transfer ownership to the target user.

Open with `O_WRONLY | O_CREAT` and `O_TRUNC` or `O_APPEND` per the
append flag. Mode 0644 (subject to umask from step 4). `dup2` to the
target fd, close the source fd. Open failure or `dup2` failure returns
`OutputFileError` (via the notification pipe).

The append flag applies uniformly to both stdout and stderr.

**Same-path optimization:** if stdout and stderr resolve to the same
path (per the path comparison method), open the file once for stdout
(fd 1) and `dup2` fd 1 to fd 2 — do not close fd 1. Both descriptors
share the same file description and write offset. `dup2` failure
returns `OutputFileError`.

> Opening all three fds as `/dev/null` in step 6, then reopening
> configured files in step 12, ensures output is captured from the
> start. Since user switching is now the caller's responsibility,
> output files are created as the current user. Use `chown_paths()`
> to transfer ownership before `drop_privileges()`.

### Pidfile mechanics

If lockfile and pidfile are the same path, seek to 0, truncate, and
write PID + `\n` to the already-locked fd from step 7. Otherwise open
with `O_WRONLY | O_CREAT | O_TRUNC`, mode 0644, write PID + `\n`, and
close. Write failure returns `PidfileError`. When not configured, no
pidfile is created. The pidfile is removed on exit when `cleanup_on_drop`
is `true` (the default); see `DaemonContext::cleanup()`.

> Lock-then-write ensures only the lock holder writes its PID,
> eliminating the race where two processes both write before either
> locks.

### Signal reset

Iterate from 1 through `libc::SIGRTMAX()`, skipping `SIGKILL` and
`SIGSTOP`. Reset each to `SIG_DFL` via `libc::sigaction()`. If
`sigaction` returns `EINVAL` (e.g., NPTL-reserved signals 32–33),
skip silently. Use `libc::sigaction()` directly — nix's `sigaction`
only accepts the `Signal` enum which cannot represent real-time signal
numbers. `libc::SIGRTMAX()` is a function; if a future `libc` version
removes it, 64 is an acceptable ceiling.

### User/group switching (DaemonContext::drop_privileges)

User/group switching is performed by `DaemonContext::drop_privileges()`,
not during the daemonization sequence. The caller controls when
privilege dropping occurs.

**Numeric ID resolution:** if the user or group string parses as a
`u32`, it is treated as a numeric UID/GID. Otherwise, resolve via
`getpwnam()` / `getgrnam()`. This matches standard Unix tool behavior
(`chown`, `su`, etc.).

**Four combinations:**

- Neither user nor group configured: no-op.
- User only: `getpwnam()` → `initgroups(username, primary_gid)` →
  `setgid(primary_gid)` → `setuid(uid)`.
- User and group: `getpwnam()` → `getgrnam()` →
  `initgroups(username, user_primary_gid)` → `setgid(group_gid)` →
  `setuid(uid)`.
- Group only: `getgrnam()` → `setgid(group_gid)`.

After switching, set `USER`, `HOME`, `LOGNAME` environment variables —
these unconditionally overwrite any `.env()` values from step 11.

`getpwnam()` failure returns `UserNotFound`. `getgrnam()` failure
returns `GroupNotFound`. `initgroups`/`setgid`/`setuid` failure returns
`PermissionDenied`.

> `initgroups()` (not bare `setgroups()`) is needed because
> `getpwnam()` only returns the primary GID; `initgroups()` consults
> the group database for supplementary groups.

### Path ownership (DaemonContext::chown_paths)

`chown_paths()` changes ownership of all configured path-based
resources (pidfile, lockfile, stdout, stderr) to the resolved target
user/group. Must be called while still privileged. Resolves user/group
using the same string-parsing strategy as `drop_privileges()`.

`chown()` failure returns `ChownError`.

### Mode and umask interaction

All file-creating opens (steps 7, 8, 13) pass mode 0644. The resulting
on-disk permissions are `0644 & !umask`. The library does not
temporarily override umask.

### Fd closing

When `close_fds` is true (the default), iterate
3..`rlimit(RLIMIT_NOFILE).rlim_cur` by brute force. Skip the lockfile
fd (from step 7, identified via `AsRawFd::as_raw_fd()` on the `Flock`)
and the notification pipe write fd. If `getrlimit` fails, panic.
Individual `close()` errors are silently ignored. `/proc/self/fd`
enumeration must not be used.

When `close_fds` is false, this step is skipped entirely.

> Brute-force is the required strategy for portability across Unix
> systems. `EBADF` is expected for most fds; `EIO` on an inherited fd
> is not actionable.
>
> Setting `close_fds` to false is useful in foreground mode where the
> caller is running under a supervisor (systemd, launchd) that passes
> file descriptors the daemon needs to keep.

---

## CLI

Binary name: `daemonize`. Version from `Cargo.toml` via
`crate_version!()`. Description: "Daemonize a program". Clap defaults
for `--help` and `--version` are acceptable.

### Flags

| Short | Long              | Argument     | Description                          |
| ----- | ----------------- | ------------ | ------------------------------------ |
| `-p`  | `--pidfile`       | path         | Pidfile path                         |
| `-c`  | `--chdir`         | path         | Working directory                    |
| `-m`  | `--umask`         | octal string | Process umask (e.g. `022`)           |
| `-o`  | `--stdout`        | path         | Redirect stdout to file              |
| `-e`  | `--stderr`        | path         | Redirect stderr to file              |
| `-a`  | `--append`        |              | Append to stdout/stderr files        |
| `-l`  | `--lock`          | path         | Lockfile path                        |
| `-E`  | `--env`           | `name=value` | Set environment variable             |
| `-u`  | `--user`          | name or uid  | Run daemon as user                   |
| `-g`  | `--group`         | name or gid  | Run daemon as group                  |
| `-f`  | `--foreground`    |              | Stay in foreground (no fork)         |
|       | `--no-close-fds`  |              | Do not close inherited fds           |
| `-v`  | `--verbose`       |              | Diagnostic output before daemonizing |

Assign all short flags explicitly to avoid collisions.

After flags: positional program path (absolute or relative) followed by
zero or more arguments. Parsed with `trailing_var_arg(true)` +
`allow_hyphen_values(true)`. `--` accepted but not required.

**`-m`** parses octal via `u32::from_str_radix(s, 8)`, converts to
`nix::sys::stat::Mode`. Invalid octal is a clap parse error.

**`-E`** splits on the first `=`. Missing `=` means empty value
(`-E FOO` = `-E FOO=`).

### Program path resolution

Before daemonization, the CLI resolves the program path to ensure it
remains valid after `chdir` changes the working directory:

- **Path contains `/`** (e.g., `./my-app`, `../bin/foo`,
  `subdir/foo`): canonicalize via `std::fs::canonicalize()`. If
  canonicalization fails, exit with `ProgramNotFound` (code 66). If
  the canonicalized path is not executable by the current euid, also
  exit `ProgramNotFound`. Store the canonicalized absolute path for
  exec.
- **Absolute path** (e.g., `/usr/bin/foo`): validate existence and
  executability. No canonicalization needed.
- **Bare name without `/`** (e.g., `my-app`): leave as-is. `execvp`
  searches PATH, which works regardless of CWD.

> POSIX `execvp` semantics: paths containing `/` are resolved relative
> to CWD; paths without `/` are searched via PATH. Since `daemonize`
> changes CWD (step 5), relative paths with `/` would resolve against
> the new CWD, not the user's original directory. Canonicalizing before
> daemonization fixes this. This also matches how daemonize(1) behaves
> — it requires an absolute path.

All CLI validation occurs before daemonization.

### Stderr derivation from stdout

When `--stdout` is given but `--stderr` is not, the CLI derives the
stderr path from the stdout path:

- If the stdout path ends in `.stdout`, stderr uses `.stderr`.
- If the stdout path ends in `.out`, stderr uses `.err`.
- Otherwise, stderr shares the same path as stdout (same fd via `dup2`).

This is a CLI-only convenience; the library requires explicit
`stdout`/`stderr` configuration.

### Program execution

The CLI calls `unsafe { daemonize(&config) }`, then:

1. If user or group is configured: calls `ctx.chown_paths()` then
   `ctx.drop_privileges()`.
2. Clears `CLOEXEC` on the lockfile fd (so the lock survives exec).
3. Execs via `execvp`. argv[0] is the (possibly canonicalized) program
   path; subsequent elements are trailing arguments in order.

The CLI does **not** call `notify_parent()`. Instead, it relies on the
notification pipe's `O_CLOEXEC` flag: successful `execvp` replaces the
process, CLOEXEC closes the pipe write end, and the parent reads EOF
(which means success per the notification protocol).

If `execvp` fails, the CLI calls `ctx.report_error(&DaemonizeError::
ExecFailed(...))`, which writes the error to the notification pipe and
calls `_exit()`. The parent reads the error and exits with the mapped
code.

If clearing `CLOEXEC` on the lockfile fd fails, the CLI likewise calls
`report_error` with `ExecFailed` (exit 71).

> This design means successful exec is automatically detected (no
> explicit write needed), and exec failure is reported to the parent
> through the pipe rather than being silently lost.

### Verbose mode

With `-v`: implementation-defined diagnostics to stderr before
daemonization. Without `-v`: no diagnostic output.

### Exit codes

This table is the single authoritative source. `DaemonizeError::
exit_code()` returns these values.

| `DaemonizeError` variant | Exit code | `sysexits.h` constant |
| ------------------------ | --------- | --------------------- |
| `ValidationError`        | 64        | `EX_USAGE`            |
| `ProgramNotFound`        | 66        | `EX_NOINPUT`          |
| `UserNotFound`           | 67        | `EX_NOUSER`           |
| `GroupNotFound`          | 67        | `EX_NOUSER`           |
| `LockConflict`           | 69        | `EX_UNAVAILABLE`      |
| `LockfileError`          | 73        | `EX_CANTCREAT`        |
| `ForkFailed`             | 71        | `EX_OSERR`            |
| `SetsidFailed`           | 71        | `EX_OSERR`            |
| `ChdirFailed`            | 71        | `EX_OSERR`            |
| `PermissionDenied`       | 77        | `EX_NOPERM`           |
| `PidfileError`           | 73        | `EX_CANTCREAT`        |
| `OutputFileError`        | 73        | `EX_CANTCREAT`        |
| `ChownError`             | 73        | `EX_CANTCREAT`        |
| `ExecFailed`             | 71        | `EX_OSERR`            |

Pre-daemonization errors: CLI prints message to stderr, exits per
table. Post-daemonization errors: reported to the parent via the
notification pipe.

---

## Testing strategy

### Panic profile constraint

`Cargo.toml` must not set `panic = "abort"` for `[profile.dev]` or
`[profile.test]`. `[profile.release]` may.

> NullForker exit-path tests use `catch_unwind`, which requires
> unwinding panics.

### Send + Sync assertions

```rust
#[test]
fn send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DaemonConfig>();
    assert_send_sync::<DaemonContext>();
    assert_send_sync::<DaemonizeError>();
}
```

### Internal Forker trait

The double-fork sequence and notification pipe are abstracted behind a
`pub(crate)` trait for deterministic unit testing:

```rust
pub(crate) trait Forker {
    fn create_notification_pipe(&mut self) -> Option<(OwnedFd, OwnedFd)>;
    fn fork(&mut self) -> Result<ForkResult, DaemonizeError>;
    fn setsid(&mut self) -> Result<(), DaemonizeError>;
    fn exit(&self, code: i32) -> !;
}
```

`daemonize()` delegates to `daemonize_inner(&DaemonConfig, &mut impl
Forker)`. `RealForker` wraps nix/libc calls (in `unsafe_ops`).
`NullForker` exists only under `#[cfg(test)]` with configurable fork
results and error flags. `NullForker::create_notification_pipe()`
returns `None` (pipe is skipped; the parent branch exits immediately).
`NullForker::exit` panics; tests use `catch_unwind`.

> The injection pattern keeps the production code path identical to the
> test path (unlike `#[cfg(test)]` swaps). The trait boundary
> encompasses the fork sequence and notification pipe; all other
> syscalls remain direct nix calls.

### Double-fork unit tests (via NullForker)

Tests call `daemonize_inner()` directly. 6 code paths:

- Both forks Child: success.
- First fork Parent: exit(0); `catch_unwind` verifies.
- First Child, second Parent: exit(0) after setsid; `catch_unwind`.
- First fork fails: `ForkFailed`.
- Setsid fails: `SetsidFailed`.
- Second fork fails: `ForkFailed`.

### Config validation tests

Tempdirs; fast, deterministic, real filesystem.

### Narrow in-process unit tests

Individual post-fork operations tested without forking. Call the
operation's `pub(crate)` function, verify via corresponding syscall
query. Tests mutating process-global state (umask, signal mask, cwd,
env) use `#[serial]`.

- **Umask:** set, read back, assert, restore.
- **Signal mask:** block signal, clear, read back, assert empty.
- **Signal dispositions:** install handler for SIGUSR1, reset, assert
  `SIG_DFL`. Repeat for SIGRTMIN.
- **Environment:** set pairs with duplicates, assert last-write-wins.
- **chdir:** change to tempdir, assert current_dir, restore.
- **fd redirect:** redirect fd 1 to tempfile, write, read back, assert.
- **fd closing:** open fds, close range with retained fd, assert
  retained open, others closed.

### CLI integration tests

Spawn CLI binary, inspect /proc and files. Require Linux, /proc:

- **Happy path:** target writes PID/PPID/SID/CWD. Assert PPID==1,
  PID!=SID, CWD matches, pidfile correct.
- **Relative path resolution:** use `./target_program` as program path.
  Assert it executes correctly despite chdir.
- **Stdout/stderr redirect:** assert file contents. Repeat with append.
- **Same-path stdout/stderr:** assert no interleaving.
- **Lockfile exclusion:** two instances, assert second exits 69.
- **User switching (root-only, skip in CI):** assert UID/GID.
- **Group switching (root-only, skip in CI):** assert GID with
  independent group.
- **Output file ownership (root-only, skip in CI):** with `-u`,
  assert stdout/stderr files are owned by the target user (via
  `chown_paths()`).
- **Foreground mode:** with `-f`, assert daemon runs in same
  process group; no fork occurs.
- **No-close-fds:** with `--no-close-fds`, assert inherited fds
  survive daemonization.
- **Error exit codes:** one test per exit code table row.
- **Verbose mode:** with/without `-v`, assert stderr content.
- **Parent notification:** assert parent does not exit until daemon
  signals readiness or exec succeeds.
- **Exec failure reporting:** use nonexistent absolute path after
  daemonization (e.g., by racing a file delete). Assert parent exits
  with code 71 and prints error message.

---

## Documentation

### Crate-level docs

Module-level doc in `lib.rs` with two usage examples: (1) simple
builder → `unsafe { daemonize() }` → `notify_parent()` → daemon
continues; (2) split-phase with `chown_paths()` → `drop_privileges()`
→ `notify_parent()`. Use `?` (not `unwrap` or `try!`); hidden
`fn main()` wrapper.

### Public item docs

- `daemonize()`: `# Safety` (threading), `# Errors` (all variants),
  `# Panics` (broken-OS conditions). Document foreground mode behavior.
- `daemonize_checked()`: `# Panics` (thread count > 1, /proc
  unavailable).
- `validate()`: `# Errors`.
- `lockfile_fd()`: CLOEXEC clearing use case, lifetime semantics.
- `chown_paths()`: must be called while privileged, no-op without
  user/group.
- `drop_privileges()`: numeric ID resolution, four user/group
  combinations, env var side effects. Document recommended call order.
- `notify_parent()`: parent exit behavior, Drop semantics.
- `report_error()`: writes error protocol and calls `_exit()`.
- All builder methods: what the field controls, default value.
- `cleanup()`: best-effort, idempotent, standalone lockfiles preserved.
- `set_cleanup_on_drop()`: runtime override for cleanup-on-drop.
- `DaemonContext`: drop releases lock, writes failure to pipe if
  `notify_parent()` was not called, removes pidfile if
  `cleanup_on_drop` is true, `#[non_exhaustive]`.
- `DaemonizeError`: each variant's condition, `exit_code()`.
  Note which variants are CLI-only.

### Cargo.toml metadata

```toml
[package]
name = "blivet"
version = "0.1.0"
edition = "2021"
description = "Daemonize a process using the double-fork method"
license = "MIT OR Apache-2.0"
repository = "<repo-url>"
keywords = ["daemon", "daemonize", "fork", "unix", "linux"]
categories = ["os::unix-apis"]
```

---

## Out of scope

- `systemd` sd_notify protocol
- POSIX capability manipulation
- Linux namespace handling
- Chroot support
- Privileged action callbacks (use split-phase `drop_privileges()` instead)
- Any form of `exec` within the library (exec is CLI-only)
- Serde support for `DaemonConfig`
- `.clear_env()` builder method

---

## Requirements checklist

The spec body is authoritative for detail and rationale. These are
verification points.

### Behavioral requirements (testable from outside)

- R1. `DaemonConfig::new()` == `Default::default()`.
- R2. Default: chdir `/`, umask 0, options `None`, append `false`,
  foreground `false`, close_fds `true`, env empty.
- R3. `.env()` accumulates; other builders are setters.
- R4. Daemon runs in a new session (SID differs from caller's).
- R5. Daemon is not the session leader (PID != SID).
- R6. Daemon is orphaned (PPID == 1).
- R7. Stdin is `/dev/null` after daemonization.
- R8. Stdout/stderr are `/dev/null` when not configured.
- R9. Configured stdout/stderr files contain expected output.
- R10. Configured output files are owned by the target user when `-u`
  is used (via `chown_paths()`).
- R11. Files are truncated when append is off.
- R12. Files are appended when append is on.
- R13. Append flag applies uniformly to both stdout and stderr.
- R14. Same-path stdout/stderr produces no interleaving corruption.
- R15. Lockfile is exclusively flocked after daemonization.
- R16. Second instance with same lockfile fails with exit code 69.
- R17. Pidfile contains PID as decimal + `\n`.
- R18. No pidfile when not configured.
- R19. Pidfile is removed on daemon exit when `cleanup_on_drop` is true
  (the default). Pidfile survives when `cleanup_on_drop` is false.
- R20. With default config, umask is 0.
- R21. With configured umask, process umask matches.
- R22. Configured chdir changes CWD.
- R23. Default CWD is `/`.
- R24. Configured env vars are present in daemon environment.
- R25. Inherited env vars are preserved.
- R26. Duplicate env keys: last-write-wins.
- R27. User switching sets UID, GID, and supplementary groups.
- R28. `USER`, `HOME`, `LOGNAME` match target user after switching.
- R29. `USER`/`HOME`/`LOGNAME` from switching overwrite `.env()` vals.
- R30. Validation rejects non-absolute paths.
- R31. Validation rejects pidfile path that is a directory.
- R32. Validation rejects chdir path that does not exist or is not a
  directory.
- R33. Validation rejects lockfile/pidfile == stdout/stderr overlap.
- R34. Validation permits lockfile == pidfile.
- R35. Validation rejects non-root caller when user or group is
  configured.
- R36. Validation rejects empty env key or key containing `=`.
- R37. Validation does not call `getpwnam()` or `getgrnam()`.
- R38. Validation errors returned before any fork.
- R39. Parent waits for daemon readiness before exiting.
- R40. Parent exits 0 after `notify_parent()` is called.
- R41. Parent exits non-zero if `DaemonContext` is dropped without
  `notify_parent()`.
- R42. Parent exits 0 when exec succeeds (EOF on pipe).
- R43. Parent exits non-zero with error message when exec fails.
- R44. Post-fork errors are reported via parent's stderr, not lost.
- R45. `daemonize_checked()` panics when thread count > 1.
- R46. `DaemonConfig` is `Send + Sync`.
- R47. `DaemonContext` is `Send + Sync`.
- R48. `DaemonizeError` is `Send + Sync`.
- R49. Each `DaemonizeError` variant maps to its exit code table entry.
- R50. CLI binary name is `daemonize`.
- R51. CLI exits with mapped exit code for each error condition.
- R52. With `-v`, diagnostic output to stderr before daemonization.
- R53. Without `-v`, no diagnostic output.
- R54. Dropping `DaemonContext` releases the lock.
- R55. CLI canonicalizes program paths containing `/` before
  daemonization.
- R56. Bare program names (no `/`) are resolved via PATH at exec time.
- R57. First fork failure returns `Err` directly, not via pipe.
- R58. Second fork failure is reported via notification pipe.
- R59. `drop_privileges()` with user only: sets UID, GID, and
  supplementary groups via user's primary GID.
- R60. `drop_privileges()` with user and group: sets UID via user,
  GID via independent group, supplementary groups via user.
- R61. `drop_privileges()` with group only: sets GID, no setuid.
- R62. `drop_privileges()` with neither: no-op.
- R63. Numeric string user/group (e.g. "1000") resolved as UID/GID.
- R64. `chown_paths()` chowns pidfile, lockfile, stdout, stderr to
  target user/group.
- R65. `chown_paths()` is a no-op when neither user nor group is set.
- R66. Foreground mode: daemon runs in current process (no fork).
- R67. Foreground mode: `notify_parent()` is a no-op.
- R68. Foreground mode: all non-fork steps still execute.
- R69. `close_fds` false: inherited fds are not closed.
- R70. Group switching sets GID independently from user's primary GID.
- R71. CLI calls `chown_paths()` then `drop_privileges()` when user
  or group is configured.
- R72. `cleanup()` removes pidfile from disk.
- R73. `cleanup()` is idempotent (second call is no-op).
- R74. `cleanup()` ignores errors (best-effort).
- R75. `cleanup()` leaves standalone lockfiles on disk.
- R76. `cleanup_on_drop` true: Drop calls `cleanup()`.
- R77. `cleanup_on_drop` false: Drop does not call `cleanup()`.
- R78. `set_cleanup_on_drop()` overrides config-level setting.

### Implementation constraints (verifiable by code review)

- R79. `DaemonConfig` derives `Default`, `Debug`, `Clone`, `Eq`,
  `PartialEq`.
- R80. `DaemonContext` is `#[non_exhaustive]`, all fields private.
- R81. `DaemonContext` derives `Debug`; not `Clone` or `PartialEq`.
- R82. `DaemonizeError` derives `Debug`, `thiserror::Error`; not
  `Clone` or `PartialEq`.
- R83. `Display` messages are lowercase, no trailing punctuation.
- R84. All builder methods: `&mut self` → `&mut Self`, infallible.
- R85. `daemonize()` calls `validate()` before forking.
- R86. `daemonize()` is `pub unsafe fn`.
- R87. `daemonize_checked()` is `pub fn`, `#[cfg(target_os = "linux")]`.
- R88. Crate root: `#![deny(unsafe_code)]`.
- R89. All `unsafe` blocks confined to `unsafe_ops` module.
- R90. `daemonize()` delegates to `daemonize_inner()` with
  `&mut impl Forker`.
- R91. `Forker` is `pub(crate)`, invisible to consumers.
- R92. `NullForker` exists only under `#[cfg(test)]`.
- R93. `Cargo.toml`: no `panic = "abort"` in dev/test profiles.
- R94. Steps execute in specified order; ordering is load-bearing.
- R95. `/dev/null` open or `dup2` failure panics.
- R96. Lockfile opened with `O_WRONLY | O_CREAT | O_CLOEXEC`, 0644.
- R97. Shared lockfile/pidfile: lock first, then seek/truncate/write.
- R98. File-creating opens use mode 0644, subject to process umask.
- R99. Signal reset: 1..SIGRTMAX, skip SIGKILL/SIGSTOP, EINVAL → skip.
- R100. Signal reset uses `libc::sigaction()` directly.
- R101. `drop_privileges()` user switch order: `getpwnam` →
  `initgroups` → `setgid` → `setuid`.
- R102. Output files opened in daemonization step 12 (before caller's
  privilege drop).
- R103. Fd closing: brute-force 3..rlim_cur, not `/proc/self/fd`.
- R104. Fd closing skips lockfile fd and notification pipe fd.
- R105. `getrlimit` failure panics.
- R106. Individual `close()` errors silently ignored.
- R107. Notification pipe both ends created with `O_CLOEXEC`.
- R108. CLI does not call `notify_parent()`; relies on CLOEXEC + exec.
- R109. CLI calls `report_error()` on exec failure.
- R110. CLI clears CLOEXEC on lockfile fd before exec.
- R111. `execvp` argv[0] is the program path as provided by user
  (possibly canonicalized).
- R112. Trailing args parsed with `trailing_var_arg` +
  `allow_hyphen_values`.
- R113. All CLI validation occurs before daemonization.
- R114. Library does not accept a program path or call exec.
- R115. Path comparison: `canonicalize()` with byte-equality fallback.
- R116. `ProgramNotFound` and `ExecFailed` are produced only by CLI.
- R117. `DaemonContext::Drop` writes exit code `1` and failure message
  to pipe if `notify_pipe` is still `Some`.
- R118. `DaemonContext::Drop` calls `cleanup()` when `cleanup_on_drop`
  is true.
- R119. Parent-side pipe reader is implemented inside `daemonize()`'s
  parent branch (step 1).
- R120. `DaemonContext` stores cloned path/user/group fields from
  config (not a config reference or full clone).
- R121. Foreground mode skips steps 1–3 (pipe, forks, setsid).
- R122. `close_fds` false skips step 13 (fd closing).
- R123. `chown_paths()` resolves user/group using same string-parsing
  strategy as `drop_privileges()`.
- R124. CLI calls `chown_paths()` then `drop_privileges()` when
  user or group is configured, between `daemonize()` and exec.
