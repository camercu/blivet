# 0003. The Android emulator tier is advisory, not a release gate

- Status: Accepted
- Date: 2026-09-15
- Supersedes: part of [0002](0002-android-support-and-test-tiers.md) — tier 3's
  standing, not its purpose
- Related: [0001](0001-capability-based-platform-gating.md)

## Context

[0002](0002-android-support-and-test-tiers.md) defines three Android tiers and
argues that tier 3, the emulator smoke test, "sits in the slow tier next to
`bsd-smoke` as a job of its own, so a failure there cancels nothing else". That
is true of other *jobs*, and it is what the tiering was reasoned about. It is
not true of releases, which the record did not consider.

`release.yml` runs on `workflow_run` with
`if: github.event.workflow_run.conclusion == 'success'`. The CI workflow's
conclusion is the conjunction of its jobs, so one job that fails takes the whole
conclusion with it. An emulator that fails to boot therefore stops
semantic-release for every commit in that push — a job the record itself calls
"the slowest job in the matrix and the likeliest to flake" holding up the
release of changes it has nothing to say about.

The repository already has a mechanism for a job that should report without
gating: `coverage` carries `continue-on-error: true`.

## Decision

`android-smoke` carries `continue-on-error: true`. It still runs on every push
and pull request, and a red result is still visible on the run; it no longer
contributes to the workflow's conclusion, and so no longer gates a release.

The Android release gate is tier 2, the bionic container: real bionic and the
real dynamic linker, on a Linux kernel, which does not flake.

## Consequences

- Android is the one Supported platform whose "the library test suite runs on
  the real OS" claim is split: what blocks is the libc and the dynamic linker,
  and the kernel and SELinux are proven but not enforced. `README.md` and
  `docs/SPEC.md` say so where they make the claim, rather than leaving a reader
  to derive it from the workflow.
- A real Android-kernel regression reaches a release. The window is one
  release: the failure is on the run for anyone reading it, and the next push
  that fixes it releases normally. This is the trade the decision buys — a
  flaky gate teaches its readers to re-run rather than to read, which costs
  more than the window does.
- The `cross test` fallback 0002 names for a too-flaky tier 3 stays available
  and is now less pressing: flakiness there is no longer a release problem.
- If tier 3 stops flaking, or the emulator action gains a retry that makes it
  dependable, this decision is worth revisiting — the gate is the stronger
  claim and it is what 0002 wanted.
