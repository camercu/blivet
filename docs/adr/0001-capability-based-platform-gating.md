# 0001. Gate platform differences on capabilities, not OS names

- Status: Accepted
- Date: 2026-09-01
- Related: [0002](0002-android-support-and-test-tiers.md)

## Context

Every platform difference in the crate is currently expressed as a `#[cfg]` on
`target_os`, and the supported-platform list is written out by hand at each
site. There are 13 such `cfg` sites — `src/lib.rs` (7), `src/context.rs` (4),
`src/thread_count.rs` (2), `src/unsafe_ops.rs` (1) — plus prose copies in
`src/lib.rs` doc comments, `README.md`, `docs/SPEC.md`, the `docs.rs` target
list in `Cargo.toml`, and a fourteenth hardcoded copy inside the doc-drift
guard itself (`src/doc_sync.rs`, `platform_list_consistent`). Adding a platform
means editing all of them and hoping none is missed; the guard compares docs
against a list that is itself hand-maintained, so it cannot detect a missing
`cfg`.

Two failure modes follow from OS-name gating, both observed:

1. **The cliff.** A target either matches the enumerated list or falls into the
   "unknown Unix" branch of *every* decision at once. Rust treats
   `target_os = "android"` as distinct from `"linux"`, so Android — a kernel
   with `/proc`, reserved real-time signals, and a readable
   `/proc/self/status` — silently took the unknown-Unix branch in all three
   places: the safe `daemonize` entry point became a `#[deprecated]` panicking
   stub, `list_open_fds` returned `None` (falling back to the
   `3..rlim_cur` close loop despite `/proc/self/fd` existing), and
   `signal_range` returned `1..=64`, resetting the real-time signals bionic
   reserves for POSIX timers, debuggerd, and the profiler — exactly the class
   of signal the Linux branch exists to skip.
2. **The unasked question.** The name of an OS does not say what it can do. The
   crate does not need to know it is on Linux; it needs to know whether a
   thread count is queryable, whether a file-descriptor directory is
   trustworthy, whether a reserved real-time signal range must be skipped, and
   whether `faccessat` accepts `AT_EACCESS`. Those four questions have
   different answers on the same OS name over time — bionic lacks `AT_EACCESS`
   today (nix gates the constant behind `#[cfg(not(target_os = "android"))]`,
   which is why the crate did not compile for Android at all) and may gain it
   via `faccessat2` tomorrow.

## Decision

**Express platform differences as named capabilities, resolved from one table.**

A build script maps each supported `target_os` to the capabilities it has, and
emits one `cargo::rustc-cfg` alias per capability (the mechanism `nix` itself
uses for `linux_android` and `freebsdlike`), declared with
`cargo::rustc-check-cfg` so a typo is a compile error rather than a silently
false branch. Source code gates on the capability alias; no module names an OS.

The initial capability set:

| Capability | Question it answers | Mechanism |
| --- | --- | --- |
| `thread_count` | Can the live thread count be read? | `/proc/self/status`, `proc_pidinfo`, `sysctl` |
| `fd_dir` | Is there a trustworthy open-fd directory? | `/proc/self/fd`, `/dev/fd` |
| `rt_signals_reserved` | Must a reserved real-time signal range be skipped? | libc-internal signals |
| `faccessat_eaccess` | Does `faccessat` accept `AT_EACCESS`? | effective-UID access check |

The same table is the single source of truth for the documented platform list:
the build script exports it as an environment variable, and the doc-drift guard
in `src/doc_sync.rs` reads that instead of its own copy. Adding a platform
becomes one row.

**Support is tiered, and the tiers are documented in `README.md` and
`docs/SPEC.md`:**

- **Supported** — the test suite runs on the real OS in CI.
- **Cross-checked** — type-checked for the target in CI, no runtime proof.
- **Best-effort** — compiles, takes conservative fallbacks, and says so.

**Best-effort stays compiling.** An unrecognised Unix target keeps every
fallback it has today. The warning that it is unproven is carried by the
existing `#[deprecated]` stub for `daemonize` / `drop_privileges`: it is
visible at compile time to *consumers*, warns by default, becomes a hard error
under `-D warnings`, and names the `unsafe` escape hatch. A build-script
`cargo::warning` is deliberately **not** the mechanism for this — Cargo only
surfaces build-script warnings for path dependencies, so a consumer building
blivet from crates.io would never see it. Non-Unix targets get an explicit
`compile_error!` naming the target, which beats today's failure mode of type
errors from deep inside `nix`.

**Capability claims are tested against reality.** Each capability gets a probe
test that asks the running system directly and asserts the answer matches the
compile-time alias — for example, that `faccessat(AT_EACCESS)` really does fail
on Android. A compile-time table is a claim about the world, and claims rot; the
probe is what turns a silent wrong assumption into a red test on the day the
platform changes.

## Consequences

- Adding a platform is one table row plus whatever probe tests fail, instead of
  13 `cfg` edits and 4 prose edits with no backstop.
- A new platform can be partially supported. Capabilities are decided one at a
  time rather than all-or-nothing, which is what Android needs.
- `cfg` aliases carry intent: `#[cfg(blivet_fd_dir)]` says why the branch
  exists; `#[cfg(target_os = "linux")]` only says where it applies.
- Cost: a `build.rs` (hand-rolled, no new dependency) that must be added to the
  `include` list in `Cargo.toml`, and one more moving part in the build.
- Diagnostics must stop hiding platform divergence. `validate_parent_writable`
  in `src/config.rs` maps every `faccessat` failure to "parent directory is not
  writable", which is how an `EINVAL` from an unsupported flag reached users as
  a false permission error. Errors from capability-dependent syscalls name the
  errno, and only genuine `EACCES` is reported as unwritable.
- Where a capability is absent and the fallback is not semantically identical,
  the difference is documented rather than silently taken — see ADR 0002 for
  the effective-UID case.

## Alternatives considered

- **Keep OS-name `cfg`s, add Android to each list.** Cheapest patch, no new
  build machinery. Rejected: it fixes one instance of a mistake the design
  invites, and leaves the next platform to rediscover the same cliff across 13
  sites.
- **The `cfg_aliases` crate.** Same mechanism, ~15 lines saved, one more
  dependency in a crate that keeps its dependency list deliberately short.
  Rejected on that trade.
- **Runtime capability detection instead of compile-time.** Would survive
  platform changes without a rebuild, but costs a probe syscall on every path,
  cannot remove code that does not compile for the target (the `AT_EACCESS`
  constant does not exist on Android), and turns compile-time impossibility
  into a runtime branch. Rejected; the probe tests recover the benefit without
  the cost.
