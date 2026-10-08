# Testability seams: specification

Status: approved, 2026-10-08. Lands before 0.15.0.

## Summary

The sequence in `src/sequence.rs` changes process-wide state through system
calls, and three kinds of bugs kept getting through review. A failed foreground
start forgot to undo a change. A step acted on the wrong resource. Code
assumed a kernel behaviour that some platform does not have. A fourth problem
made all three slower to catch. Mutants in root-only code survive every
unprivileged run, so each one needed a hand check in the Linux root container.

This slice adds five things, in order:

1. Mutation testing inside the privileged Linux container, locally and in CI.
2. Undo tokens. Every step that changes process state returns its own undo.
3. One record of the files the sequence opened. The chown targets and the
   descriptors the fd sweep spares both come from it.
4. Kernel-fact probes. Each probe asserts one operating-system behaviour the
   code depends on.
5. An explicit launcher state in `DaemonContext`, in place of reading
   `notify_pipe.is_some()` as "the launcher still waits".

The slice changes no public API and no behaviour in `docs/SPEC.md` R1 to R167.

## Architecture

### What stays as it is

The system calls stay real. The slice adds no `Sys` trait and no nullable
wrapper for the kernel. The syscalls are what blivet sells, and a stub of one
encodes the same assumption as the code under test. That would have hidden
the argc 0 crash, the close-on-exec race, and the macOS `shutdown` order. The
existing seams cover failure injection. They are `Forker` with `NullForker`,
the flags in `steps::failpoints`, and the pure `plan_output_redirect`. Kernel
behaviour is proven only by the real-OS tiers. Those tiers are the host suite,
`just docker-test`, `just termux-test`, `just package-test`, and the BSD VM
smoke jobs.

### Mutants in the root container

`just mutants-diff` runs on the host. On macOS it cannot compile procfs code,
and on any unprivileged host it cannot run the root tests. A new recipe runs
the same diff-scoped mutation test inside the image `just docker-test` builds,
as root, with the ignored tests included. The container has no `.git`,
because `just build-context` sends only working-tree files. So the recipe
computes the diff on the host and passes it into the container. The CI
`mutants` job switches to the new recipe. The host recipe stays for a fast
local loop.

### Undo tokens

Today `AbortGuard` holds a typed field per step and a method per field
(`note_umask`, `save_cwd`, `note_mask`, `note_env`, `save_groups`). A new step
needs a new field, a new method, and a new line in `Drop`. Forgetting any of
the three is the K1 bug class. `SavedSlot` already shows the better shape. The
step returns a value that restores the old state when it drops, unless
something calls `keep()`. Every state-changing step moves to that shape. The
change and its undo then become one value, so a step cannot apply one without
the other. `AbortGuard` holds the tokens in the order the steps ran and drops
them in reverse. In daemon mode, under `Rollback::Skip`, a token holds nothing
and restores nothing.

The pidfile and the lock stay where R164 puts them. The pidfile is removed
before the lock is released, in both modes.

Steps that cannot be undone return no token. Step 13 closes inherited
descriptors, and step 15 is the caller's `init`. Step 16 chowns files, and
R164 does not restore their owners.

### The walk test

A test failpoint fails the sequence just after the Nth token is created. The
walk test runs a foreground start with N = 1, then 2, and so on, until the
start succeeds. After each failed run it compares a snapshot of process state
with the snapshot taken before. The snapshot covers the umask, the working
directory, what fds 0 to 2 point at, every signal disposition, the signal
mask, the environment, the supplementary groups, the real and effective gid,
and whether the pidfile exists.

The walk needs no list of steps. A future step that changes state without
returning a token changes the snapshot at every N after it, and the test
fails. A final failpoint just before step 18 covers a change made after the
last token. The unprivileged tier walks a start with no identity switch. The
root tier walks one with a user, so steps 14, 16, and 17 run too.

### One record of opened files

Steps 7, 8, and 12 open the lockfile, the pidfile, and the stream files. Two
later decisions read what they opened, and today each assembles its own list.
Step 13's fd sweep must spare those descriptors (R152), and step 16 chowns
them (R163). The F5 fix had to add the stream files to both lists by hand. A
single record of opened files replaces both lists. Each entry holds the path,
the role, and the owned descriptor. Two pure functions read the record. One
returns the descriptors to spare. The other returns the chown targets. The
input type is the record, and stdio slots cannot appear in it, so fd 1 and fd
2 cannot become chown targets.

The descriptors that tokens hold, such as a saved stdio slot or the saved
working directory, also go to the spare list. Those come from the tokens, not
from the record.

### Kernel-fact probes

`src/capability_probes.rs` checks each capability claim in `build.rs`
against the running system. A new test module, `src/kernel_facts.rs`, does the
same for the behaviours the code assumes on every platform. Each probe names
the code that depends on its fact. The probes run in every tier that runs the
library tests. If a platform changes one of these behaviours, its probe fails
and names the code to revisit.

### Launcher state

`DaemonContext` stores the notification pipe as `Option<NotifyPipe>`, and
three decisions read `None` as "the launcher was told": whether a failure
removes the pidfile whatever `cleanup_on_drop` says (R154), whether an error
goes to stderr (R161), and whether `Drop` writes to the pipe. Any path that
used up the pipe without telling the launcher that the start succeeded
looked like a started daemon. Commits `51646e4` and `d4f7ff2` fixed two such
paths, one at a time.

A private enum replaces the option:

```rust
enum Launcher { Waiting(NotifyPipe), Released, Foreground }
```

`Waiting` holds the pipe while the launcher waits. A successful readiness
send moves it to `Released`. `notify_parent` also moves it to `Released` on
failure, because it hands the error to a caller that may keep running.
`notify_parent_or_report` leaves it `Waiting` on failure, so the report that
follows sees a start that never succeeded. A failed send has spent the pipe,
so that report's message must still reach stderr (R161), not vanish into a
write to nothing. `Foreground` has no launcher.
Each decision matches on the enum, so a new variant fails to compile until
each decision handles it.

## 1. Mutants in the root container

- **1.1**: `just mutants-docker base` mutation-tests the Rust lines changed
  since `base`, as `just mutants-diff base` selects them. Uncommitted and
  untracked files count.
- **1.2**: The tests for each mutant run as root in the image `just
  docker-test` builds, with ignored tests included. The run fails if it
  is not root, as `scripts/privileged-test.sh` fails.
- **1.3**: A mutant counts as caught, missed, timed out, or unviable by the
  same rules as `just mutants-diff`. A timed-out mutant counts as caught.
- **1.4**: `just mutants-docker` exits non-zero if any mutant is missed, and
  leaves `mutants.out` on the host.
- **1.5**: The image pins the `cargo-mutants` version that the nixpkgs
  revision in `nix/pinned.nix` provides, as it pins `cargo-nextest`. A comment
  beside the pin says to bump both together.
- **1.6**: The CI `mutants` job runs `just mutants-docker` with the base it
  selects today, and uploads `mutants.out`.
- **1.7**: `just mutants-docker` with no diff argument sweeps the whole
  crate, for the release checklist in `docs/RELEASE-1.0.md`.
- **1.8**: A mutant in code that only root can exercise, killed by a root-tier
  test, does not appear as missed in CI.

## 2. Undo tokens

- **2.1**: Each step from 4 to 17 that changes state a caller can see, and that
  R164 restores, returns a token. A dropped token restores what its step
  changed, unless `keep()` was called on it.
- **2.2**: The tokens cover the umask, the working directory, the stdio slots
  of steps 6 and 12, the signal dispositions, the signal mask, the environment
  variables, and the supplementary groups. Step 17 keeps undoing its own
  `setgid` when `setuid` fails, inside the step.
- **2.3**: A token is `#[must_use]`.
- **2.4**: On failure, the sequence restores tokens in the reverse of the order
  their steps ran.
- **2.5**: Under `Rollback::Skip`, a token saves nothing and restores
  nothing.
- **2.6**: Step 18 keeps every token, so a successful start leaves every change
  in place.
- **2.7**: `AbortGuard` has no per-step field and no per-step method. Adding a
  step that returns a token changes no code in `AbortGuard`.
- **2.8**: The pidfile is removed before the lock is released, in both modes,
  as R164 requires.
- **2.9**: The walk test fails a foreground start after each token in turn,
  and after the last step before 18. After each failure, process state equals
  the state before the start, as the Architecture section's snapshot defines
  it.
- **2.10**: The walk test runs unprivileged without an identity switch, and in
  the root tier with a user switch.
- **2.11**: The walk test needs no list of steps. A new step that changes
  snapshot state without a token fails it.

## 3. One record of opened files

- **3.1**: The sequence keeps one record of the files that steps 7, 8, and 12
  opened. Each entry holds the path, the role, and the owned descriptor.
- **3.2**: The descriptors step 13 spares are a pure function of the record,
  the tokens' held descriptors, and the notification pipe's write end.
- **3.3**: The chown targets of step 16 are a pure function of the record and
  `chown_paths`. The function cannot take a stdio slot as a target.
- **3.4**: Each pure function has table tests that need no subprocess and no
  root.
- **3.5**: Every descriptor the record or a token holds is spared by step 13.

## 4. Kernel-fact probes

- **4.1**: `src/kernel_facts.rs` holds one test per fact. Each test names the
  code that depends on its fact.
- **4.2**: A `send` with `MSG_NOSIGNAL` to a socket whose peer has closed fails
  with `EPIPE`, and raises no `SIGPIPE`. Dependent code is
  `notify::ChannelWriter` (R165).
- **4.3**: On a socket pair, `shutdown(SHUT_RD)` on one end followed by
  `shutdown(SHUT_WR)` on the other succeeds. Dependent code is
  `notify::channel`.
- **4.4**: A two-byte `send` on an `AF_UNIX` stream socket arrives whole: one
  `recv` of one byte, then a second `recv` of one byte, both succeed without
  waiting. Dependent code is `notify::read_outcome` and the protocol section
  of `docs/SPEC.md`.
- **4.5**: `F_DUPFD_CLOEXEC` returns a descriptor at or above its floor, with
  close-on-exec set. Dependent code is `steps::above_stdio` (R152, R107).
- **4.6**: The action that `sigaction` reports as replaced restores the
  original disposition exactly, handler and flags included. Dependent code is
  the signal-disposition token (R164).
- **4.7**: The probes run in every tier that runs the library tests, and pass
  on every supported platform.

## 5. Launcher state

- **5.1**: `DaemonContext` holds its launcher state as `Waiting` (with the
  pipe), `Released`, or `Foreground`, and holds no separate
  `Option<NotifyPipe>`.
- **5.2**: The pidfile cleanup in `report_error` and `Drop` removes the
  pidfile when the state is `Waiting`. Otherwise it removes it only if
  `cleanup_on_drop` is `true` (R154).
- **5.3**: A successful readiness send moves the state to `Released`. After
  a failed one, `notify_parent` moves the state to `Released` and returns the
  error.
- **5.4**: After a failed readiness send, `notify_parent_or_report` leaves
  the state `Waiting`. The report that follows removes the pidfile whatever
  `cleanup_on_drop` says, prints the error on stderr (R161), and exits 71.
- **5.5**: Each decision that depends on the launcher (pidfile cleanup, the
  stderr message of R161, the write in `Drop`, the marker in `exec`) is an
  exhaustive `match` on the state, with no wildcard arm.
- **5.6**: The behaviour in `docs/SPEC.md` R1 to R167 and the `Debug` output
  of `DaemonContext` do not change.

## Out of scope

- A `Sys` trait or nullable wrapper over system calls.
- Running mutants for code compiled only on macOS or a BSD. CI runs mutants on
  Linux only, as it does today.
- Restoring a chowned file's owner, or inherited descriptors that step 13
  closed.
- Turning the step sequence itself into data run by an executor.
- Changes to the CLI program and `argv[0]` resolution.

## Open questions

None.
