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
- [ ] **Final pass over the [Rust API guidelines checklist]** against
      `public-api.txt`. Confirm no pre-1.0 dependency types (nix) leak
      into the public surface.
- [ ] **Re-bless and audit `public-api.txt`** one last time; every line
      is a 1.0 commitment.

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
- [ ] **Green CI on all jobs**, including the VM smoke tier (FreeBSD,
      OpenBSD, NetBSD, OmniOS) and Docker root tests.
- [ ] **Docs current**: README, SPEC, man page, CLI `--help`, and
      docs.rs rendering (all four configured targets) reviewed against
      actual behavior.

## Release mechanics

- [ ] **Flip semantic-release breaking rule** in `.releaserc.json` from
      `{ "breaking": true, "release": "minor" }` to major. Sequencing:
      flip the rule first, then land the `LockConflict` restructure as
      the breaking commit — semantic-release then computes 1.0.0 from
      0.x automatically. No manual version edit needed.
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
