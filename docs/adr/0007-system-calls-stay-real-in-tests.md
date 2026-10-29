# 0007. System calls stay real in tests

- Status: Accepted
- Date: 2026-10-28
- Related: [0001](0001-capability-based-platform-gating.md),
  [0003](0003-the-emulator-tier-is-advisory.md),
  [0006](0006-the-notification-channel-cannot-raise-sigpipe.md)

## Context

The daemonize sequence in `src/sequence.rs` changes process-wide state
through system calls. A failed foreground start must put every change back,
newest first ([R164](../SPEC.md)). Each step that changes state returns an
undo token, an `Undo` from `src/steps/undo.rs` that holds a
`Box<dyn Restore>`. `AbortGuard` in `src/sequence.rs` holds the tokens and
drops them in reverse. Tests prove the rollback in three ways:

- Isolated tests (`src/test_support.rs`) run one start in a fresh process,
  so a change to the umask, the cwd, or the signal mask cannot leak into
  the test harness.
- The undo walk (`src/undo_walk.rs`) fails the start after each token in
  turn. It compares a snapshot of process state with the one taken before.
- Failpoints in `steps::failpoints` make a step fail where the kernel will
  not.

The testability-seams spec (commit `049c3a1`, approved 2026-10-08) added
the undo tokens, the walk, one record of opened files, kernel-fact probes,
and an explicit launcher state. It also rejected a `Sys` trait and a
nullable wrapper for the kernel. Its reason was that a stub of a system
call encodes the same assumption as the code under test. Code review that
reasoned about the kernel instead of running it missed three bugs, and
each showed up only on a real kernel:

- `exec` with an empty argv, which crashes some programs (`a9b9af9`).
- A subprocess spawned between `socketpair` and the `fcntl` that set
  close-on-exec inherited a channel end (`0b429cd`).
- macOS fails `shutdown` with `ENOTCONN` once the peer has shut down its
  writing side, so the order of the two calls matters (`notify::channel`).

`src/sys.rs` holds plain wrappers for raw calls. It is not a seam.

The nullable infrastructure pattern (James Shore, *Testing Without Mocks*)
promises fast in-process tests of edge cases without mocks. In October
2026 a spike tested whether it makes blivet simpler to test and to change.
The slice was the four process-state steps (`set_umask`, `change_dir`,
`clear_signal_mask`, `set_env_vars`) and their rollback. To measure the
cost of a change, each design also added a new step, `set_priority`, after
`set_env_vars`. The spike ran six designs in two rounds, and a fresh agent
reviewed and ranked round 2:

| Round | Design | Shape | Result |
| --- | --- | --- | --- |
| 1 | baseline | the current tokens, walk, and failpoints | reference |
| 1 | generic | a trait with `Real` and `Null` impls, steps generic over it | lost |
| 1 | embedded | a concrete wrapper whose lowest call a stub can replace | lost |
| 2 | data-tokens | a plain-data enum of saved state, real calls only | ranked first |
| 2 | fault-wrapper | a real-call wrapper that can be told to fail | lost |
| 2 | contract-nullable | a stub swapped in for tests, guarded by a contract test | lost |

A contract test runs the same assertions on the real calls and on a stub,
so the stub cannot claim behaviour the kernel does not have. A probe breaks
the code on purpose and records which tests notice. The branch
`spike/nullable-infra` holds the findings (`nullable-infra-findings.md`,
both rounds) and the round-2 review (`review-r2.md`). The branches
`spike/nullable-infra-<design>` hold each design.

These results decided it:

- **A stub catches no kernel bug.** One probe made the real `fchdir` a
  no-op. In baseline and in each round-2 design, 3 tests that run on the
  real kernel failed. None of contract-nullable's 6 in-process rollback
  tests, which run on its stub, failed.
- **Stubs drift from the kernel.** The embedded stub let an unprivileged
  process lower its nice value, which the kernel refuses with `EACCES`.
  The generic design's contract test caught that drift. contract-nullable
  has a contract test too, but its stub still claims rules that its
  contract does not pin. For example, its stub opens any directory. The
  kernel refuses to open a directory the process can search but not read,
  and R164 covers that case: the start goes on without saving the cwd.
- **Injected faults can claim failures the kernel cannot produce.**
  fault-wrapper's change-task test injects `EACCES` on *raising* nice, and
  an unprivileged process may always raise its nice value.
- **The real kernel already supplies the edge cases.** data-tokens reaches
  the `setpriority` rollback by lowering nice, which the kernel refuses an
  unprivileged process. That path needs no failpoint and no stub. It does
  depend on the host: on Linux with `RLIMIT_NICE` raised, lowering
  succeeds.
- **The other round-2 designs cost more.** Production code grew by 137
  lines (contract-nullable) and 154 lines (fault-wrapper), against 103 for
  data-tokens. contract-nullable puts 3 layers between a step and its
  system call (4 in test builds), where main has none on the forward path.
  It adds 217 lines of test-only stub and splits the step order across a
  closure.

## Decision

System calls stay real. Production code has no `Sys` trait, no nullable
wrapper, and no wrapper that injects faults. Kernel behaviour is proven by
the real-OS tiers: the host suite, `just docker-test`, `just termux-test`,
`just package-test`, and the CI jobs `bsd-smoke`, `netbsd-smoke`, and
`omnios-smoke`. The `android-smoke` emulator job also runs a real kernel,
but it is advisory (ADR 0003).

The failpoints and the pure planners (`plan_output_redirect`,
`spared_fds`, `chown_targets`) stay as they are. A failpoint makes a real
call path fail. It does not fake kernel state.

A test double for a system call is allowed only where the real call cannot
run soundly in the test process. Each new double comes with a contract
test that runs the same assertions on the real call and on the double. A
double claims no behaviour that its contract does not pin.

`NullForker` is the one double today, and it predates this record. A test
cannot call `fork` in the multithreaded test harness, because `fork` in a
multithreaded process is undefined behaviour. `NullForker` returns a
configured fork result without forking, so `daemonize_inner` runs in
process. It models no kernel rule beyond which branch of the fork the
caller takes, so it has no contract to pin. The CLI tests and
`tests/daemonize.rs` run the real `fork`.

The spike also considered splitting the whole sequence into a pure plan
and a separate executor. That idea was rejected: steps pass data to later
steps, the fork splits the sequence in two, and `init` returns the
caller's own type. The three pure planners above already plan at the grain
where planning fits.

## Consequences

- `docs/specs/testability-seams.md` goes away. Its five parts are
  implemented, and commit `049c3a1` keeps its text. This record keeps the
  reasoning behind its rejection of a `Sys` trait.
- Rollback tests keep the subprocess cost. The undo walk stays the check
  that finds a step that changes state without returning a token.
- Three parts of the data-tokens design carry over to `main`, each as its
  own change:
  - Landed: each isolated test is one function, in place of a pair: an
    `#[ignore]` body and a test that ran it by name in a subprocess. The
    spike used a macro. `test_support::isolated` is a plain function
    instead, so rustfmt still formats the test bodies.
  - Landed: a plain-data enum of saved state, `steps::Saved`, with
    exhaustive matches for restoring, for `single_threaded`, and for
    `held_fd`, replaces the `Box<dyn Restore>` in each token. The enum
    stays inside `Undo`, so `#[must_use]` still catches a dropped token.
    Whether a token restores only while single-threaded is a match on the
    variant, not a flag that `Undo::single_threaded` set.
  - Restoring returns a `Result`, and rollback collects the failures.
    Every error variant carries them as structured data. An error from
    `init` keeps its own message (R154), because the failures go in a
    separate field. An end-to-end test asserts the report. Today a failed
    restore leaves no trace.
- The contract-test technique stays in reserve for the day a new double is
  needed.
