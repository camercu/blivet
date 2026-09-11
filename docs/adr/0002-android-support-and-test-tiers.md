# 0002. Android is a supported platform, proven by tiered CI

- Status: Accepted
- Date: 2026-09-01
- Related: [0001](0001-capability-based-platform-gating.md)

## Context

Before this decision the crate did not compile for Android at all:

```
error[E0599]: no associated item named `AT_EACCESS` found for struct `AtFlags`
  --> src/config.rs:465
```

`nix` gates `AtFlags::AT_EACCESS` behind `#[cfg(not(target_os = "android"))]`
because bionic's `faccessat` rejects the flag. PR #14 ("handle android",
external contributor, tested on a Snapdragon 685) fixed the build break by
falling back to `AtFlags::empty()` on Android. That the break survived to a
release is the diagnostic finding: `just check-cross` type-checks Linux,
FreeBSD, and NetBSD, so no gate ever asked whether Android compiled, and no CI
job has ever run a line of this crate on bionic.

Android is not an exotic ask for a daemonizing CLI — Termux is a normal home
for one — but "it built and the pidfile appeared on my phone" is not a support
claim the project can make on a contributor's behalf. Either the platform is
tested and documented as supported, or it is a target that merely compiles.
Shipping neither statement is the outcome to avoid.

Fidelity matters when choosing how to test it. The behaviours that differ on
Android are bionic's (`faccessat` flag handling, the reserved real-time signal
range, `getpwnam` resolving a built-in AID table rather than `/etc/passwd`) and
the Android kernel's (SELinux, writable locations). No single cheap environment
exercises both.

## Decision

**Android becomes a Supported platform under ADR 0001's tiering**: its
capabilities are declared in the table, the safe `daemonize` and
`drop_privileges` entry points are enabled there, and the test suite runs on
bionic in CI.

Capability answers for Android: `thread_count` yes (`/proc/self/status`),
`fd_dir` yes (`/proc/self/fd`), `rt_signals_reserved` yes (bionic reserves the
first real-time signals for POSIX timers, debuggerd, and the profiler — they
must be skipped, exactly as the NPTL range is on glibc), and
`faccessat_lacks_eaccess` yes — Android is the only listed platform whose
`faccessat` rejects the flag, which is why the capability is named for the
deviation.

**The effective-UID gap is documented, not papered over.** With `AT_EACCESS`
unavailable, the writability probe on Android tests the *real* UID. `nix` also
offers no `eaccess` there, and the alternatives — a raw `faccessat2` syscall,
or hand-computing permissions — buy little for a check that is advisory and
inherently racy. The fallback stands, and the rustdoc on `DaemonConfig::validate`,
the comment in `validate_parent_writable`, and the path rules in `docs/SPEC.md`
(which today state the check is against the current euid) all state the Android
exception. On Android the distinction is close to theoretical — app-accessible
filesystems are mounted `nosuid` — but the documentation must not claim a
guarantee the code does not make.

**Testing is tiered by cost and fidelity**, mirroring the existing fast/slow
split in `.github/workflows/ci.yml`:

1. **Type-check, every PR (fast tier).** `aarch64-linux-android` and
   `x86_64-linux-android` join `just check-cross`. `cargo check` needs only
   the rustup `rust-std` for the target — no NDK — so this is nearly free, and
   it is the gate that would have caught the original break.
2. **Unit tests on bionic, every PR (fast tier).** The suite runs inside the
   `termux/termux-docker:x86_64` container, alongside the existing
   `docker-test` job. This is real bionic and the real dynamic linker on a host
   Linux kernel: it proves the libc-level divergences and, as Termux, matches
   how the CLI is actually deployed on a phone. It does not prove Android
   kernel behaviour and has no Android runtime components.
3. **End-to-end smoke on a real emulator (slow tier).** `cargo-ndk` builds the
   binary and test binary; `reactivecircus/android-emulator-runner` on
   `ubuntu-latest` boots an x86_64 system image with KVM enabled via a udev
   rule (hardware acceleration is available on standard hosted runners); the
   artefacts are pushed to `/data/local/tmp` and run over `adb shell`. This is
   the only tier with a real Android kernel and SELinux, and it is where the
   daemonize sequence itself — fork, setsid, pidfile, signal — is proven. It sits
   in the slow tier next to `bsd-smoke` as a job of its own, so a failure there
   cancels nothing else.

`cross test --target aarch64-linux-android` (QEMU-based, test support listed as
working by cross-rs) is the fallback for tier 3 if the emulator proves too slow
or flaky in practice; it trades the real kernel for far less plumbing.

## Consequences

- The crate can state Android support in `README.md` and `docs/SPEC.md` and
  stand behind it, and Android joins the `docs.rs` target list in `Cargo.toml`.
- Android consumers get the safe `daemonize` entry point rather than a
  `#[deprecated]` stub that forces `unsafe { daemonize_unchecked(...) }`.
- CI grows two jobs. The container job is cheap; the emulator job is the
  slowest in the matrix and the most likely to flake, which is why it sits in
  the slow tier and why the QEMU fallback is named in advance.
- The test suite must become location-independent first. Roughly 28 sites
  hardcode `/tmp` (`src/config.rs` 17, `src/steps.rs` 7, `src/unsafe_ops.rs` 2,
  `src/context.rs` 1, `src/lib.rs` 1), and several pass it to `validate()`,
  which stats the real path. Android and Termux have no `/tmp`, so those tests
  would fail for a reason unrelated to what they assert. A shared temp-directory
  helper replaces the literals.
- `getpwnam`/`getgrnam` resolve differently on bionic (built-in AID table, no
  `/etc/passwd`). The identity tests move from assumption to evidence by
  running in tiers 2 and 3.
- `public-api.txt` is generated for the host target, so enabling APIs on
  Android does not churn the snapshot; the platform-conditional surface remains
  invisible to that gate.
