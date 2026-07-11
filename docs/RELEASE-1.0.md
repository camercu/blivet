# 1.0 release checklist

Work items to close before cutting 1.0.0, in order. The guiding rule:
1.0 freezes the public API — any change we'd regret making
non-breaking-able must land first.

## API freeze decisions

- [x] **Settle error payload shapes.** `#[non_exhaustive]` lets us add
      variants after 1.0, but changing an existing variant's payload is
      breaking. Decision: restructure `LockConflict` to carry the
      conflicting path (`LockConflict { path: PathBuf }`) so callers can
      handle already-running programmatically; all other `String`
      payloads stay display-only by contract. Document that contract on
      the enum ("match on the variant and `exit_code()`, not on payload
      contents").
- [x] **Final pass over the [Rust API guidelines checklist]** against
      `public-api.txt`. Done 2026-07-10. Confirmed no pre-1.0 dependency
      types (nix) leak into the public surface — every payload/return
      type is std/core/alloc. Findings applied: `Hash` derived on
      `DaemonConfig` (C-COMMON-TRAITS; adding later is non-breaking but
      free now), `documentation` link added to Cargo.toml (C-METADATA).
      Accepted as-is: variant suffix mix (`*Failed` for syscall verbs,
      `*Error` for file/domain nouns) — renaming is breaking churn with
      no consumer value; variant-level `#[non_exhaustive]` declined —
      the enum-level attribute already allows new variants, which is the
      escape hatch if a variant ever needs richer payload.
- [x] **Re-bless and audit `public-api.txt`** — done 2026-07-10 with the
      pinned nightly; snapshot had no drift, then re-blessed after the
      `Hash` derive. Every line audited as a 1.0 commitment.

[Rust API guidelines checklist]: https://rust-lang.github.io/api-guidelines/checklist.html

## Quality gates

- [x] **Full `cargo mutants` sweep** over the whole crate with zero
      missed mutants (or each miss triaged and either tested or
      documented as unreachable). Done 2026-07-10 on macOS (host) plus
      Linux-with-root (Docker, `--run-ignored all`); equivalent and
      cfg-dead mutants are excluded in `.cargo/mutants.toml`. Residual
      known misses, accepted as documented:
      - fd-redirect internals (`redirect_to_devnull` bound,
        `execute_stream_action` arms): observable only with fds 0-2
        re-plumbed; the CLI/Docker integration tier exercises the
        behavior end to end.
      - per-OS `thread_count` implementations: each is testable only
        on its own OS; the host-OS path is covered by
        `current_thread_count_tracks_live_threads` on every CI OS.
      - `reset_signal_dispositions` internals (`||`, RT-signal `!=`):
        verifying per-signal dispositions needs a dedicated subprocess
        harness; candidate for a future test slice.
      - `raw_initgroups` errno boundary (`< 0`): needs root plus a
        forced initgroups failure.
- [x] **Green CI on all jobs**, including the VM smoke tier (FreeBSD,
      OpenBSD, NetBSD, OmniOS) and Docker root tests. Verified
      2026-07-10: run 29120479022 on trunk (811687c), all 12 jobs green
      (commitlint skipped — PR-only). Re-confirm on the final pre-release
      push.
- [x] **Docs current**: README, SPEC, man page, CLI `--help`, and
      docs.rs rendering (all four configured targets) reviewed against
      actual behavior. Done 2026-07-10. Fixes: SPEC error tables gained
      the missing `NotifyFailed`/`PrivilegesNotDropped`/`Application`
      rows, SPEC CLI description matched to actual `--help`, README
      `signal-hook` crate name corrected. Man page exit-code table
      verified complete (70 correctly absent — unreachable from the
      CLI). All four docs.rs targets build docs with `-D warnings`.

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
