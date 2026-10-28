# 1.0 release checklist

Work items to close before cutting 1.0.0, in order. The guiding rule:
1.0 freezes the public API — any change we'd regret making
non-breaking-able must land first.

The API freeze decisions, the full mutants sweep, the API guidelines pass,
and the 2026-07-10 docs review are done. `git log -p -- docs/RELEASE-1.0.md`
holds their notes.

## Before the cut

- [ ] **Green CI on all jobs** on the final pre-release push, including the
      VM smoke tier and the Docker root tests.
- [ ] **Docs current again**: README, SPEC, man page, CLI `--help`, and
      docs.rs on all configured targets, reviewed against behaviour since
      the 2026-07-10 review.

## Release mechanics

- [ ] **Flip semantic-release breaking rule** in `.releaserc.json` from
      `{ "breaking": true, "release": "minor" }` to major.
      **Sequencing note (2026-07-10):** the original plan — flip the
      rule, then land the `LockConflict` restructure as the breaking
      commit — is stale: that restructure already shipped in 0.11.0
      (breaking→minor). A different trigger for the 1.0.0 computation is
      needed: either a deliberate `feat!:` release commit (e.g. declaring
      the stability contract) after the flip, or a manual version edit.
      Decide before flipping.
- [ ] **Write the post-1.0 SemVer policy** into README or
      CONTRIBUTING: breaking = major, MSRV bump policy (currently
      1.85; state whether MSRV bumps are minor or major).
- [ ] **Add `cargo semver-checks` to CI** so post-1.0 accidental
      breakage is caught mechanically, not just by the public-api
      snapshot diff.

## After the cut

- [ ] Verify crates.io publish, docs.rs build on all configured
      targets, and the GitHub release notes.
- [ ] Announce/update any downstream users on the lockfile-derivation
      and foreground-error behavior changes (0.11.0) plus the 1.0
      error-shape change.
