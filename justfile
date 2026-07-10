# Environment variable: set CARGO_LOCKED=--locked in CI for reproducibility
locked := env("CARGO_LOCKED", "")

# cargo driver. Defaults to plain `cargo`; set RTK_CARGO="rtk cargo" (see the
# `ci-rtk` target) to route the compile-heavy recipes through rtk for
# token-compressed output. Only used where rtk both compresses the subcommand
# and the output is for reading — recipes whose output is consumed (public-api)
# stay on plain cargo.
cargo := env("RTK_CARGO", "cargo")

# Set up development environment (pre-commit hooks, node deps)
setup:
    ./scripts/setup-dev.sh

# Build all targets including tests
build:
    {{cargo}} build {{locked}} --tests

# Check formatting
fmt-check:
    cargo fmt --check

# Run clippy lints
lint:
    {{cargo}} clippy {{locked}} -- -D warnings

# Run cargo-deny checks (advisories, licenses, bans)
lint-deny:
    cargo deny check

# Build documentation (warnings are errors)
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc {{locked}} --no-deps

# Cross-check the non-host Unix targets CI smoke-tests, catching platform
# type differences (e.g. rlim_t is i64 on FreeBSD, u64 elsewhere) before
# push. `cargo check` needs only the target's std (rustup-installable);
# OpenBSD is tier-3 without one, so CI's OpenBSD smoke remains the backstop.
check-cross:
    rustup target add x86_64-unknown-linux-gnu x86_64-unknown-freebsd x86_64-unknown-netbsd
    {{cargo}} check {{locked}} --target x86_64-unknown-linux-gnu
    {{cargo}} check {{locked}} --target x86_64-unknown-freebsd
    {{cargo}} check {{locked}} --target x86_64-unknown-netbsd

# Run all static checks
check: fmt-check lint lint-deny doc check-cross

# Run tests (excludes ignored root/Linux tests)
test:
    RUSTFLAGS="-D warnings" {{cargo}} test {{locked}}

# Build and run Docker container for root + Linux-specific tests
docker-test:
    docker build -t blivet-test .
    docker run --rm --init --privileged blivet-test

# Regenerate manpage from markdown source (requires pandoc).
# The @VERSION@ placeholder is filled from Cargo.toml's package version, so the
# man-page version is never hand-maintained.
manpage:
    @version=$(grep -E '^version = ' Cargo.toml | head -1 | sed -E 's/.*"(.*)".*/\1/'); \
    sed "s/@VERSION@/$version/" docs/daemonize.1.md | pandoc -f markdown -s -t man -o docs/daemonize.1

# Generate code coverage report (requires cargo-llvm-cov + cargo-nextest).
# Runs under nextest, not the plain `cargo test` harness: several tests have
# process-global side effects (redirect/close std fds) that clobber the shared
# harness's result pipe, failing the run with a BrokenPipe. nextest isolates
# each test in its own process, so those tests can't corrupt the collector.
coverage:
    cargo llvm-cov nextest --html {{locked}}
    @echo "Coverage report: target/llvm-cov/html/index.html"

# ── Public API surface ──────────────────────────────────────
# cargo-public-api builds rustdoc JSON, which is nightly-only. The nightly is
# pinned because rustdoc's rendering changes across nightlies (e.g. io::Error
# moving from std to core paths), which would show up as false snapshot
# drift. Bump the pin deliberately and re-bless the snapshot in the same
# commit. cargo-public-api itself is pinned via shell.nix for the same
# reason.
public_api_nightly := "nightly-2026-07-10"

# Install the pinned nightly used by the public-api recipes (used by CI).
public-api-toolchain:
    rustup toolchain install {{public_api_nightly}} --profile minimal

# Print the current public API surface (--simplified omits blanket/auto-trait
# impl noise, keeping the snapshot readable and stable across toolchains).
public-api:
    cargo +{{public_api_nightly}} public-api --simplified

# Regenerate the committed public API snapshot after an intended change.
public-api-bless:
    cargo +{{public_api_nightly}} public-api --simplified > public-api.txt

# Fail if the public API has drifted from the committed snapshot.
public-api-check:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo +{{public_api_nightly}} public-api --simplified | diff -u public-api.txt - \
        || { echo "public API drifted from public-api.txt — review, then run 'just public-api-bless'"; exit 1; }

# Run everything CI runs (except Docker)
ci: check test

# Agent-facing CI: same steps as `ci`, but routes the compile-heavy recipes
# (build/clippy/check/test) through rtk for token-compressed output. Prefer this
# over `ci` when an agent runs the suite. Same pass/fail semantics.
ci-rtk:
    RTK_CARGO="rtk cargo" just ci

# Run the full CI suite including Docker tests
ci-full: check test docker-test

# Run semantic-release (used by release workflow)
release:
    npm ci
    npx semantic-release
