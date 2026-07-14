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

> The public API exposes no third-party types. `umask()` takes a plain
> `u32` octal value (range-checked in `validate()`), so callers need no
> `nix` dependency or version match to set a umask. `nix` types stay
> internal to the implementation.

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

The crate root uses `#![deny(unsafe_code)]`; modules opt back in locally with
`#[allow(unsafe_code)]` only where required. Raw libc/syscall FFI whose
invariants can be upheld internally is encapsulated as safe `pub(crate)`
wrappers in one `unsafe_ops` module. The remaining `unsafe` is for operations
whose safety is *context-dependent* — they require single-threadedness the
caller establishes — so they live at the call site that owns that contract
rather than behind a fake-safe wrapper.

`unsafe` appears in these places:

- `unsafe_ops`: the encapsulated FFI wrappers — signal reset
  (`libc::sigaction()`, incl. the real-time range via `SIGRTMIN()`/
  `SIGRTMAX()`), `close()`, `initgroups()`, `_exit()`, `unlink()`, `raise()`,
  and the per-OS thread-count queries.
- `forker.rs` / `lib.rs`: `RealForker::fork` is an `unsafe fn` wrapping
  `nix::unistd::fork()` (`unsafe` since nix 0.24). `daemonize_inner` is itself
  an `unsafe fn` that invokes it as `unsafe { forker.fork() }`, forwarding the
  single-threaded contract to its callers — `daemonize_unchecked`, or the
  checked `daemonize` after its guard.
- `lib.rs`: `daemonize_unchecked()` is a public `unsafe fn` (caller contract),
  and the safe `daemonize()` calls it via `unsafe { … }` after the guard.
- `context.rs`: `DaemonContext::drop_privileges_unchecked()` is a public
  `unsafe fn` that calls `setenv` (`USER`/`HOME`/`LOGNAME`); the safe
  `drop_privileges()` calls it via `unsafe { … }` after the guard.
- `steps.rs`: `set_env_vars` calls `setenv` in the post-fork sequence — sound
  because the child is single-threaded by `fork` (or, in foreground mode, the
  entry point required single-threadedness).

Test code additionally uses `nix::sys::signal::sigaction`. An auditor who
reads these sites sees every `unsafe` in the crate.

---

## Public API

### DaemonConfig

`DaemonConfig` is a plain data struct with `&mut self` builder methods.
It derives `Default`, `Debug`, `Clone`, `Eq`, `PartialEq`, and `Hash`.
`DaemonConfig::new()` returns the same value as `Default::default()`.

All fields are private; callers use builder methods.

| Field        | Type                    | Default              | Builder method       | Builder input type         | Semantics   |
| ------------ | ----------------------- | -------------------- | -------------------- | -------------------------- | ----------- |
| `pidfile`    | `Option<PathBuf>`       | `None`               | `.pidfile(path)`     | `impl Into<PathBuf>`       | Setter      |
| `chdir`      | `PathBuf`               | `PathBuf::from("/")` | `.chdir(path)`       | `impl Into<PathBuf>`       | Setter      |
| `umask`      | `u32`                   | `0`                  | `.umask(mode)`       | `u32` (octal, `<= 0o7777`) | Setter      |
| `stdout`     | `Option<PathBuf>`       | `None`               | `.stdout(path)`      | `impl Into<PathBuf>`       | Setter      |
| `stderr`     | `Option<PathBuf>`       | `None`               | `.stderr(path)`      | `impl Into<PathBuf>`       | Setter      |
| `append`     | `bool`                  | `false`              | `.append(bool)`      | `bool`                     | Setter      |
| `lockfile`   | `LockfileSetting` (private tri-state) | derive from `pidfile` | `.lockfile(path)` / `.no_lockfile()` | `impl Into<PathBuf>` / none | Setter      |
| `user`       | `Option<String>`        | `None`               | `.user(name)`        | `impl Into<String>`        | Setter      |
| `group`      | `Option<String>`        | `None`               | `.group(name)`       | `impl Into<String>`        | Setter      |
| `foreground` | `bool`                  | `false`              | `.foreground(bool)`  | `bool`                     | Setter      |
| `close_fds`  | `bool`                  | `true`               | `.close_fds(bool)`   | `bool`                     | Setter      |
| `cleanup_on_drop` | `bool`             | `true`               | `.cleanup_on_drop(bool)` | `bool`                 | Setter      |
| `chown_paths` | `bool`                 | `true`               | `.chown_paths(bool)` | `bool`                     | Setter      |
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

let mut ctx = daemonize(&config)?;
// ... application initialization (still privileged if run as root) ...
ctx.notify_parent()?;
// daemon process continues here
```

**Split-phase privilege dropping** (when user/group switching is needed):

```rust
let mut config = DaemonConfig::new();
config.pidfile("/var/run/foo.pid").user("nobody").group("nogroup");

let mut ctx = daemonize(&config)?;
// privileged work here (e.g., bind port 80)
ctx.drop_privileges()?;                 // chown pidfile/lockfile/logs, then setgid + setuid
ctx.notify_parent()?;
```

### DaemonContext

`daemonize()` returns `Result<DaemonContext, DaemonizeError>`.
`DaemonContext` is `#[non_exhaustive]` with all private fields. It has a
manual `Debug` impl (see below). Do not derive `Clone` or `PartialEq`.

The context carries the validated `DaemonConfig` whole rather than
mirroring each post-daemonization setting into a parallel field. So the
pidfile, lockfile, stdout, stderr, user, group, `cleanup_on_drop`, and
`chown_paths` values are read from `config`, not stored separately.

| Field                | Type                | Semantics                                          |
| -------------------- | ------------------- | -------------------------------------------------- |
| `config`             | `DaemonConfig`      | The validated config, cloned in whole. Single source of truth for pidfile, lockfile, stdout/stderr, user/group, `cleanup_on_drop`, and `chown_paths`. |
| `lockfile`           | `Option<Flock>`     | Owned lock (`nix::fcntl`); drop releases.          |
| `notify_pipe`        | `Option<NotifyPipe>`| Write end of notification pipe; see below.         |
| `cleaned_up`         | `bool`              | Internal flag; prevents double cleanup.            |
| `privileges_dropped` | `bool`              | Set once `drop_privileges()` completes; gates `notify_parent()` (see R125). |

**Debug output:** the manual `impl` does not print `config` verbatim.
It renders `lockfile` as `held`/`none` and `notify_pipe` as
`open`/`none` (never the raw fd), then flattens the relevant config
values as pseudo-fields for readability: `pidfile`, `lockfile_path`
(the effective lockfile), `stdout`, `stderr`, `user`, `group`, and
`cleanup_on_drop`.

**Accessors:**

`lockfile_fd()` returns `Option<BorrowedFd<'_>>` (lifetime tied to the
context). Returns `None` when no lockfile was held (none configured,
none derived, or locking disabled via `no_lockfile()`).

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

**Privilege method:**

`drop_privileges(&mut self) -> Result<(), DaemonizeError>` first chowns
the configured paths (unless disabled, see below), then performs
user/group switching.

The chown phase changes ownership of pidfile, lockfile, stdout, and
stderr files to the resolved target user/group, while still privileged.
Skips files that are not configured. No-op if neither user nor group is
configured. Disabled by `config.chown_paths(false)` — for hardening
(keep files owned by the original privileged user) or custom ownership.
On error, paths already processed remain chowned; the chown is
idempotent, so retrying after fixing the cause is safe. A chown failure
aborts the drop before any `setgid`/`setuid`, so the caller is still
privileged and can remediate. The chown follows symlinks (`chown(2)`,
not `lchown`), matching how these files are opened, so configured paths
must live in directories not writable by untrusted users — otherwise a
planted symlink could redirect the privileged chown onto an arbitrary
file.

The switch phase resolves each spec — if the string parses as a `u32`,
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

`notify_parent(&mut self) -> Result<(), DaemonizeError>` writes a success
byte (`0x00`) to the notification pipe and closes it. A pipe write error
is returned as `DaemonizeError::NotifyFailed` (exit code 71) so callers
get one error type with a preserved exit code. The companion
`notify_parent_or_report(&mut self)` does the same but reports the error
to the parent and `_exit`s instead of returning it. The parent reads the
success byte and exits 0. After this call, `notify_pipe` is `None`. This
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
| `ProgramNotFound`  | CLI-only: program missing or not executable (pre-fork path check, or `ENOENT`/`EACCES` at exec) |
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
| `ExecFailed`       | CLI-only: exec of target program failed (other than `ENOENT`/`EACCES`) |
| `NotifyFailed`     | Writing the readiness byte to the notification pipe failed |
| `PrivilegesNotDropped` | User/group configured but `drop_privileges()` never called before `notify_parent()` |
| `Application`      | Caller-reported failure during the privileged init window (via `application()`/`report_error`) |

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
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd",
          target_os = "netbsd", target_os = "openbsd"))]
pub fn daemonize(
    config: &DaemonConfig,
) -> Result<DaemonContext, DaemonizeError>
```

The recommended entry point. Reads the current process's thread count,
panicking (with a message naming the problem and fix) unless it is exactly
1, or if it cannot be determined, then calls
`unsafe { daemonize_unchecked(config) }`. A count other than 1 — including
an anomalous 0 a healthy process can never report — fails closed rather than
forking on an untrusted count. The thread count source is per-OS:

| OS      | Source                                                      |
| ------- | ----------------------------------------------------------- |
| Linux   | `/proc/self/status` `Threads:` line                         |
| macOS   | `proc_pidinfo(PROC_PIDTASKINFO)` → `pti_threadnum`          |
| FreeBSD | `sysctl(KERN_PROC_PID)` → `kinfo_proc.ki_numthreads`        |
| NetBSD  | `sysctl(KERN_PROC2/KERN_PROC_PID)` → `kinfo_proc2.p_nlwps`  |
| OpenBSD | `sysctl(KERN_PROC_PID \| KERN_PROC_SHOW_THREADS)`; count = bytes / record |

On any other target it is a `#[deprecated]` stub that panics (no
thread-count source). The thread-count FFI lives in `unsafe_ops`.

> With a single thread, the observation "thread count is 1" is stable:
> incrementing it requires an existing thread to call `pthread_create`,
> but no other thread exists. The check-then-fork sequence has no race.

### daemonize_unchecked()

```rust
/// # Safety
///
/// No other threads may be running when this function is called.
/// Forking a multithreaded process leaves mutexes held by other
/// threads permanently locked in the child, causing deadlocks or
/// undefined behavior. Call before spawning threads, async runtimes,
/// or libraries with background threads.
pub unsafe fn daemonize_unchecked(
    config: &DaemonConfig,
) -> Result<DaemonContext, DaemonizeError>
```

The `unsafe` escape hatch, available on all Unix targets. Used by
`daemonize` internally, and called directly on platforms where `daemonize`
is unavailable (or when the caller manages the single-threaded contract).

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
- In foreground mode every error (steps 4–14 included) returns `Err`
  to the caller: no fork happened, so the caller is still the running
  process and the library never exits it.

The library does not accept a program path or call exec.

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

No configured path (pidfile, stdout, stderr, lockfile, chdir) may
contain a NUL byte — such a path is rejected up front rather than
surfacing as a late `EINVAL` when first passed to a syscall. Pidfile,
stdout, stderr, and lockfile paths must be absolute when configured.
Chdir path must be absolute, must exist, and must be a directory.
Pidfile path must not be a directory. Parent directories of the
configured *file* paths (pidfile, stdout, stderr, lockfile) must be
writable (checked against current euid). Chdir is exempt: it must
already exist as a directory, so its parent is never created.

### Path comparison

All overlap and sameness checks use `std::fs::canonicalize()` on both
operands. If either path does not exist, fall back to byte-equal
`PathBuf` comparison.

> Overlap detection is best-effort for not-yet-created files. The
> actual failure mode (fd interference) would produce a clear runtime
> error.

### Lockfile derivation

The lockfile setting is tri-state: **derive** (default), an **explicit
path** from `.lockfile(path)`, or **disabled** from `.no_lockfile()`.
The last call wins between `.lockfile(path)` and `.no_lockfile()`. In
the derive state the effective lockfile is the pidfile itself (if one
is configured), so a lone pidfile enforces a single instance; a second
instance fails with `LockConflict` instead of silently overwriting the
pidfile. `no_lockfile()` writes the pidfile without any flock, for
callers whose exclusivity is enforced elsewhere. Elsewhere in this
spec (including Path rules above), "lockfile" means this resolved
(effective) path.

> Locking the pidfile itself is established practice: FreeBSD's
> pidfile(3) locks the pidfile via flopen(3), and the Rust `daemonize`
> crate flocks its pid file unconditionally. daemonize(1) writes an
> unlocked pidfile; blivet follows the locking lineage and keeps
> `no_lockfile()` as the escape hatch for the unlocked behavior.

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

In foreground mode, step 6 only redirects stdin to `/dev/null`.
Stdout and stderr are left inherited so output reaches the parent
terminal or supervisor. If stdout/stderr paths are explicitly
configured, step 12 still redirects them.

Step failures in foreground mode return `Err` to the caller instead
of the write-to-pipe-and-`_exit()` behavior described per step below
(see Error paths, R134). Steps completed before the failure remain in
effect — the caller resumes in a partially configured process (umask,
chdir, stdin redirect, and env changes may already have happened).

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
6. **Redirect standard streams to `/dev/null`.** Always redirects
   stdin. Redirects stdout and stderr only in daemon mode (not
   foreground). See /dev/null redirect policy.
7. **Open and lock lockfile** (if configured or derived from the
   pidfile — see Lockfile derivation). Open with
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
13. **Close inherited fds** (if `close_fds` is true). Close the
    enumerated open fds (or iterate 3..`rlim_cur` where enumeration
    is unavailable), skipping lockfile fd and notification pipe fd.
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

Post-fork errors in steps 2–13 are written to the notification pipe
per the error protocol and the process calls `_exit()` with the mapped
exit code. In foreground mode there is no pipe and no `_exit()`;
failures return `Err` (see Foreground mode). Every fallible step maps
its failure to a named `DaemonizeError` variant: system-call failures
without a more specific variant — opening `/dev/null`, `sigprocmask`,
`getrlimit` — map to `SystemError` (exit 71). Individual `close()`
errors in step 13 are silently ignored.

> The `_exit()` code is not observable (the parent reads its exit code
> from the pipe), but uses the mapped code for consistency in process
> accounting. As a backstop, the notification-pipe write end reports a
> failure if it is dropped without any message being sent — e.g. a
> panic unwinding past the signalling seam. This stops the closed pipe
> from being read as EOF = success, which would report a crashed daemon
> as a clean start; `panic = "abort"` must therefore stay off (R93) so
> the unwind reaches the `Drop`. See R137.

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

Step 6 always redirects stdin to `/dev/null`. In daemon mode (not
foreground), it also redirects stdout and stderr. In foreground mode,
stdout and stderr are left inherited so output reaches the parent
terminal or supervisor.

Open `/dev/null` with `O_RDWR`, `dup2` to the target fd(s). If the
source fd already equals the target, skip `dup2` and close. Failure to
open `/dev/null` or `dup2` to it reports `SystemError` via the
notification pipe.

> A system without `/dev/null` (e.g. a minimal container that never
> mounted `/dev`) is unusual, but the failure is surfaced to the parent
> so the operator sees why startup failed rather than the process
> crashing with a false success reaching the parent.

### Output file redirect

Step 12 opens configured stdout/stderr files and redirects the
corresponding fds. Since user switching is now the caller's
responsibility (via `drop_privileges()`), files are created as the
current user (typically root); `drop_privileges()` chowns them to the
target user before switching.

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
> output files are created as the current user; `drop_privileges()`
> chowns them to the target user before switching.

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

Iterate from 1 through `libc::SIGRTMAX()`, skipping `SIGKILL`,
`SIGSTOP`, and `SIGPIPE`. Reset each to `SIG_DFL` via
`libc::sigaction()`. If `sigaction` returns `EINVAL` (e.g.,
NPTL-reserved signals 32–33), skip silently. Use `libc::sigaction()`
directly — nix's `sigaction` only accepts the `Signal` enum which
cannot represent real-time signal numbers. `libc::SIGRTMAX()` is a
function; if a future `libc` version removes it, 64 is an acceptable
ceiling.

**SIGPIPE is preserved, not reset.** The Rust runtime installs
`SIG_IGN` for SIGPIPE so writes to a closed pipe/socket return `EPIPE`
instead of killing the process. Resetting it to `SIG_DFL` would
silently revoke that guarantee for the whole daemon — the first write
to a disconnected peer would terminate it — and would make
`notify_parent`'s documented `NotifyFailed` error unobservable when
the parent has died (the daemon would die of SIGPIPE inside the
write). The daemonization sequence therefore leaves the caller's
SIGPIPE disposition untouched. The CLI restores `SIG_DFL` immediately
before `exec` so target programs still start with the conventional
disposition (an ignored SIGPIPE would otherwise survive `exec(2)`).

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

**Thread-count guard.** The `USER`/`HOME`/`LOGNAME` writes use `setenv`,
which is not thread-safe. So — mirroring `daemonize`/`daemonize_unchecked`
— the safe `drop_privileges()` reads the kernel thread count and panics
unless exactly 1 thread is running, but **only when a user switch is
configured** (the sole `setenv` path; a group-only switch performs no
`setenv` and is not guarded). The unchecked `unsafe fn
drop_privileges_unchecked()` performs the switch without the check, for
callers that uphold the single-threaded contract themselves or run on a
target without a thread-count source — where the checked `drop_privileges()`
is a `#[deprecated]` stub (see [Unsafe boundary](#unsafe-boundary)).

`getpwnam()` failure returns `UserNotFound`. `getgrnam()` failure
returns `GroupNotFound`. `initgroups`/`setgid`/`setuid` failure returns
`PermissionDenied`.

> `initgroups()` (not bare `setgroups()`) is needed because
> `getpwnam()` only returns the primary GID; `initgroups()` consults
> the group database for supplementary groups.

### Path ownership (drop_privileges chown phase)

Before switching user/group, `drop_privileges()` changes ownership of
all configured path-based resources (pidfile, lockfile, stdout, stderr)
to the resolved target user/group, while still privileged. Resolves
user/group using the same string-parsing strategy as the switch phase.
Disabled by `config.chown_paths(false)`.

`chown()` failure returns `ChownError` and aborts the drop before any
`setgid`/`setuid`.

### Mode and umask interaction

All file-creating opens (steps 7, 8, 13) pass mode 0644. The resulting
on-disk permissions are `0644 & !umask`. The library does not
temporarily override umask.

### Fd closing

When `close_fds` is true (the default), close every open fd except
0-2, the lockfile fd (from step 7, identified via
`AsRawFd::as_raw_fd()` on the `Flock`), and the notification pipe
write fd. Open fds are enumerated via the platform fd directory —
`/proc/self/fd` on Linux, `/dev/fd` on macOS — when available; where
no reliable listing exists (the BSDs' `/dev/fd` covers only 0-2
without fdescfs) or reading it fails (e.g. minimal containers without
`/proc`), fall back to brute-force iteration of
3..`rlimit(RLIMIT_NOFILE).rlim_cur`. If `getrlimit` fails in the
fallback, report `SystemError`. Individual `close()` errors are
silently ignored.

When `close_fds` is false, this step is skipped entirely.

> Earlier revisions required brute force and forbade `/proc/self/fd`
> for portability. That held until `RLIMIT_NOFILE` grew: systemd
> commonly configures 1M+, and `RLIM_INFINITY` clamps to `i32::MAX`,
> turning the loop into billions of `close` calls that stall daemon
> startup for seconds to hours. Enumeration is now preferred exactly
> where it is reliable, and the brute-force loop remains the fallback
> everywhere else, so portability is unchanged. `EBADF` is expected
> when the fallback closes unopened fds; `EIO` on an inherited fd is
> not actionable.
>
> Setting `close_fds` to false is useful in foreground mode where the
> caller is running under a supervisor (systemd, launchd) that passes
> file descriptors the daemon needs to keep.

---

## CLI

Binary name: `daemonize`. Version from `Cargo.toml` via clap's
`version` attribute. Description: "Run a program as a Unix daemon".
Clap defaults for `--help` and `--version` are acceptable.

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
|       | `--no-lock`       |              | Do not lock the pidfile              |
| `-E`  | `--env`           | `name=value` | Set environment variable             |
| `-u`  | `--user`          | name or uid  | Run daemon as user                   |
| `-g`  | `--group`         | name or gid  | Run daemon as group                  |
| `-f`  | `--foreground`    |              | Stay in foreground (no fork)         |
| `-v`  | `--verbose`       |              | Diagnostic output before daemonizing |

Assign all short flags explicitly to avoid collisions.

After flags: positional program path (absolute or relative) followed by
zero or more arguments. Parsed with `trailing_var_arg(true)` +
`allow_hyphen_values(true)`. `--` accepted but not required.

**`-m`** parses octal via `u32::from_str_radix(s, 8)` and passes the
`u32` to `.umask()`. Invalid octal, or a value wider than `0o7777`, is a
clap parse error.

**`-E`** splits on the first `=`. Missing `=` means empty value
(`-E FOO` = `-E FOO=`).

**`-l`/`--no-lock`** map onto the library's tri-state lockfile setting
(see Lockfile derivation): `-l path` calls `.lockfile(path)`,
`--no-lock` calls `.no_lockfile()`, and neither flag leaves the default
derive-from-pidfile state, so a pidfile alone enforces a single
instance. The two flags conflict (clap usage error). The CLI does not
implement a separate derivation rule; its `-v` output re-states the
library's resolution purely for diagnostic display.

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
  searches PATH, which works regardless of CWD. A bare name that is
  missing or not executable is therefore only discovered at exec time;
  the resulting `ENOENT`/`EACCES` still exits `ProgramNotFound` (66) —
  see Program execution.

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

The CLI calls `unsafe { daemonize_unchecked(&config) }`, then:

1. If user or group is configured: calls `ctx.drop_privileges()`
   (which chowns the configured paths first).
2. Clears `CLOEXEC` on the lockfile fd (so the lock survives exec).
3. Execs via `execvp`. argv[0] is the (possibly canonicalized) program
   path; subsequent elements are trailing arguments in order.

The CLI does **not** call `notify_parent()`. Instead, it relies on the
notification pipe's `O_CLOEXEC` flag: successful `execvp` replaces the
process, CLOEXEC closes the pipe write end, and the parent reads EOF
(which means success per the notification protocol).

If `execvp` fails, the CLI calls `ctx.report_error(...)`, which writes
the error to the notification pipe and calls `_exit()`. The parent
reads the error and exits with the mapped code. `ENOENT` (the program,
or a script's interpreter, does not exist) and `EACCES` (the program is
not executable) are reported as `ProgramNotFound` (exit 66), matching
the pre-fork path check, so a missing-or-unusable program is exit 66 in
both path and bare form; any other exec error is `ExecFailed` (exit 71).
(R130)

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
| `NotifyFailed`           | 71        | `EX_OSERR`            |
| `PrivilegesNotDropped`   | 70        | `EX_SOFTWARE`         |
| `Application`            | caller's `code`; 0 remapped to 70 | (caller's choice) |

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
Forker)`. `RealForker` (in `forker.rs`) wraps the real `fork`/`setsid`/pipe
syscalls.
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
  `SIG_DFL`. Repeat for SIGRTMIN. For SIGPIPE, assert the pre-reset
  disposition (both `SIG_IGN` and `SIG_DFL`) survives unchanged.
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
  assert stdout/stderr files are owned by the target user (chowned
  by `drop_privileges()`).
- **Foreground mode:** with `-f`, assert daemon runs in same
  process group; no fork occurs.
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
builder → `daemonize()` → `notify_parent()` → daemon
continues; (2) split-phase with `drop_privileges()` →
`notify_parent()`. Use `?` (not `unwrap` or `try!`); hidden
`fn main()` wrapper.

### Public item docs

- `daemonize()`: `# Panics` (thread count != 1, or thread count
  undeterminable). Document the per-OS thread-count source and the
  deprecated stub on unsupported targets.
- `daemonize_unchecked()`: `# Safety` (threading), `# Errors` (all
  variants), `# Panics` (broken-OS conditions). Document foreground mode
  behavior.
- `validate()`: `# Errors`.
- `lockfile_fd()`: CLOEXEC clearing use case, lifetime semantics.
- `drop_privileges()`: numeric ID resolution, four user/group
  combinations, env var side effects, the chown phase and its
  `chown_paths(false)` opt-out. Document recommended call order.
- `DaemonConfig::chown_paths()`: default `true`; disable to keep files
  owned by the original privileged user or manage ownership yourself.
- `notify_parent()`: parent exit behavior, Drop semantics.
- `report_error()`: writes error protocol and calls `_exit()`.
- All builder methods: what the field controls, default value.
- `cleanup()`: best-effort, idempotent, standalone lockfiles preserved.
- `cleanup_on_term_signals()` / `cleanup_on_signals(&[i32])`: opt-in,
  install async-signal-safe handlers that `unlink` the pidfile and
  re-raise (SA_RESETHAND) so the default termination action still runs.
  Library-only — the CLI's `exec` resets handlers. No-op without a
  pidfile; `ValidationError` on a NUL path or an uncatchable/invalid
  signal. A failed install names the failing signal and rolls back
  completely (R129).
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
description = "A correct, full-featured Unix daemon library and CLI for Rust"
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
- R7. Stdin is `/dev/null` after daemonization (all modes).
- R8. Stdout/stderr are `/dev/null` when not configured (daemon mode).
  In foreground mode, stdout/stderr are inherited when not configured.
- R9. Configured stdout/stderr files contain expected output.
- R10. Configured output files are owned by the target user when `-u`
  is used (chowned by `drop_privileges()`).
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
- R45. `daemonize()` panics when thread count is not exactly 1.
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
- R64. `drop_privileges()` chowns pidfile, lockfile, stdout, stderr to
  target user/group before switching.
- R65. The chown phase is a no-op when neither user nor group is set.
- R65a. `config.chown_paths(false)` disables the chown phase; the
  switch still happens.
- R65b. A chown failure aborts `drop_privileges()` before any
  `setgid`/`setuid` (caller stays privileged).
- R66. Foreground mode: daemon runs in current process (no fork).
- R67. Foreground mode: `notify_parent()` is a no-op.
- R68. Foreground mode: all non-fork steps still execute.
- R69. `close_fds` false: inherited fds are not closed.
- R70. Group switching sets GID independently from user's primary GID.
- R71. CLI calls `drop_privileges()` (which chowns configured paths
  first) when user or group is configured.
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
- R85. `daemonize_unchecked()` calls `validate()` before forking.
- R86. `daemonize_unchecked()` is `pub unsafe fn`.
- R87. `daemonize()` is `pub fn` on linux/macos/freebsd/netbsd/
  openbsd; a `#[deprecated]` panic stub on other targets.
- R88. Crate root: `#![deny(unsafe_code)]`.
- R89. Raw FFI is concentrated in `unsafe_ops`; the only `unsafe` outside
  it is the `fork()` call in `forker.rs` and `daemonize_unchecked` / its
  call sites in `lib.rs`.
- R90. `daemonize()` delegates to `daemonize_inner()` with
  `&mut impl Forker`.
- R91. `Forker` is `pub(crate)`, invisible to consumers.
- R92. `NullForker` exists only under `#[cfg(test)]`.
- R93. `Cargo.toml`: no `panic = "abort"` in dev/test profiles.
- R94. Steps execute in specified order; ordering is load-bearing.
- R95. `/dev/null` open or `dup2` failure reports `SystemError` via
  the notification pipe.
- R96. Lockfile opened with `O_WRONLY | O_CREAT | O_CLOEXEC`, 0644.
- R97. Shared lockfile/pidfile: lock first, then seek/truncate/write.
- R98. File-creating opens use mode 0644, subject to process umask.
- R99. Signal reset: 1..SIGRTMAX, skip SIGKILL/SIGSTOP/SIGPIPE,
  EINVAL → skip.
- R100. Signal reset uses `libc::sigaction()` directly.
- R101. `drop_privileges()` user switch order: `getpwnam` →
  `initgroups` → `setgid` → `setuid`.
- R102. Output files opened in daemonization step 12 (before caller's
  privilege drop).
- R103. Fd closing: brute-force 3..rlim_cur, not `/proc/self/fd`.
- R104. Fd closing skips lockfile fd and notification pipe fd.
- R105. `getrlimit` failure (fd-close fallback) reports `SystemError`.
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
- R121. Foreground mode skips steps 1–3 (pipe, forks, setsid) and
  leaves stdout/stderr inherited in step 6.
- R122. `close_fds` false skips step 13 (fd closing).
- R123. The `drop_privileges()` chown phase resolves user/group using
  the same string-parsing strategy as the switch phase.
- R124. CLI calls `drop_privileges()` (chowning configured paths first)
  when user or group is configured, between `daemonize()` and exec.
- R125. When a user/group is configured but `drop_privileges()` was
  never called, `notify_parent()` returns `PrivilegesNotDropped`
  (exit 70) instead of signaling readiness — so a daemon can never
  report itself healthy while still holding elevated privileges.
- R126. `drop_privileges()` panics when a user switch is configured and
  the thread count is not exactly 1 (the user-switch path calls `setenv`,
  which is not thread-safe). The unchecked `unsafe fn
  drop_privileges_unchecked()` skips the check; on targets without a
  thread-count source the checked form is a `#[deprecated]` stub.
- R127. The caller's SIGPIPE disposition is preserved across
  daemonization: the signal reset never touches SIGPIPE (see signal
  reset), so the Rust runtime's `SIG_IGN` — and with it `EPIPE`-on-write
  semantics — survives `daemonize()`.
- R128. The CLI sets SIGPIPE to `SIG_DFL` immediately before `exec`, so
  the target program starts with the conventional default disposition.
- R129. `cleanup_on_signals()` is all-or-nothing: if installing a
  handler fails, dispositions already replaced for earlier signals in
  the slice — and the handler's pidfile path — are restored before the
  error (which names the failing signal) is returned, so an `Err`
  leaves the process exactly as it was.
- R130. CLI exec failure with `ENOENT` (missing program or script
  interpreter) or `EACCES` (program not executable) is reported as
  `ProgramNotFound` (exit 66), matching the pre-fork path check — so a
  missing-or-unusable program is exit 66 in both path and bare form. Any
  other exec error is `ExecFailed` (exit 71).
- R131. With a pidfile configured and the lockfile setting in its
  default (derive) state, the pidfile itself is exclusively flocked;
  no pidfile means no derived lockfile.
- R132. `no_lockfile()` disables locking: the pidfile is written
  without a flock and no lockfile is created.
- R133. `.lockfile(path)` and `.no_lockfile()` are last-call-wins.
- R134. In foreground mode, setup errors (steps 4–14) return `Err` to
  the caller; the library never exits the caller's process.
- R135. With `close_fds` true, open fds are enumerated via the
  platform fd directory where reliable (Linux `/proc/self/fd`, macOS
  `/dev/fd`); enumeration failure degrades to the brute-force
  3..rlim_cur fallback, never to skipping the step.
- R136. Validation rejects any configured path containing a NUL byte.
- R137. A daemon process that exits without signaling readiness —
  including a panic that unwinds past the signalling seam — reports a
  failure to the parent via the notification pipe, never a closed pipe
  the parent reads as EOF = success.
