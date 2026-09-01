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

# Type-check the non-host Unix targets, catching platform type differences
# (rlim_t is i64 on FreeBSD, u64 elsewhere) and libc constants a platform
# lacks (bionic has no AT_EACCESS) before push. `cargo check` needs only the
# target's std (rustup-installable); OpenBSD is tier-3 without one, so CI's
# OpenBSD smoke remains the backstop there.
check-cross:
    rustup target add x86_64-unknown-linux-gnu x86_64-unknown-freebsd x86_64-unknown-netbsd aarch64-linux-android x86_64-linux-android
    {{cargo}} check {{locked}} --target x86_64-unknown-linux-gnu
    {{cargo}} check {{locked}} --target x86_64-unknown-freebsd
    {{cargo}} check {{locked}} --target x86_64-unknown-netbsd
    {{cargo}} check {{locked}} --target aarch64-linux-android
    {{cargo}} check {{locked}} --target x86_64-linux-android

# Lowest rustc that must be able to *resolve* the dev-dependency graph: the
# rust OpenBSD ships via `pkg_add rust` (Tier 3, no rustup), which the openbsd
# smoke job runs `cargo test --lib` under. Bump when OpenBSD packages a newer
# rust; that is also what unblocks the serial_test ignore in dependabot.yml.
openbsd_rust := "1.90"

# Fail if a resolved dependency declares a rust-version too high for the
# toolchains CI must satisfy. Two floors, because the failure modes differ:
#
#   runtime deps (normal + build) — must fit this crate's MSRV, or consumers
#       on that MSRV cannot build blivet at all.
#   dev-dependencies — need only fit {{openbsd_rust}}, the oldest rustc that
#       runs our test suite. Over-declaring is common (heapless declares 1.87
#       yet compiles on 1.85), so holding dev-deps to the crate MSRV would
#       reject working graphs.
#
# Cargo is not a backstop for either: the toolchain the msrv job pins (1.85)
# predates the resolver diagnostic and silently compiled serial_test 4.x
# (rust-version 1.93.1) — only OpenBSD's 1.90 cargo, which does enforce,
# caught it. Reading metadata directly holds on every toolchain.
msrv-check:
    #!/usr/bin/env bash
    set -euo pipefail
    msrv=$(cargo metadata --format-version 1 --no-deps \
        | jq -r '.packages[] | select(.name == "blivet") | .rust_version')
    [ -n "$msrv" ] && [ "$msrv" != "null" ] || { echo "no rust-version in Cargo.toml"; exit 1; }
    runtime=$(cargo tree -e normal,build --prefix none {{locked}} \
        | awk 'NF >= 2 { sub(/^v/, "", $2); print $1 "@" $2 }' | sort -u)
    offenders=$(cargo metadata --format-version 1 {{locked}} \
        --filter-platform "$(rustc -vV | sed -n 's/^host: //p')" \
        | jq -r '.packages[] | select(.rust_version != null) | "\(.name)@\(.version) \(.rust_version)"' \
        | while read -r pkg req; do
            if grep -qxF "$pkg" <<<"$runtime"; then
                floor=$msrv kind=runtime
            else
                floor={{openbsd_rust}} kind=dev
            fi
            [ "$(printf '%s\n%s\n' "$floor" "$req" | sort -V | head -1)" = "$req" ] \
                || echo "  $pkg ($kind) requires rustc $req > $floor"
        done)
    if [ -n "$offenders" ]; then
        echo "dependencies exceed the supported rustc floor:"
        echo "$offenders"
        echo "pin the dependency back, or raise the floor deliberately"
        exit 1
    fi
    echo "dependencies fit MSRV $msrv (runtime) and {{openbsd_rust}} (dev)"

# Run all static checks
check: fmt-check lint lint-deny doc msrv-check check-cross

# Run tests (excludes ignored root/Linux tests)
test:
    RUSTFLAGS="-D warnings" {{cargo}} test {{locked}}

# Build and run Docker container for root + Linux-specific tests
docker-test:
    docker build -t blivet-test .
    docker run --rm --init --privileged blivet-test

# Run the library tests against bionic, Android's libc, in a Termux container.
# The platform is pinned to linux/amd64 so a developer on any architecture runs
# what CI runs; off an x86_64 host that means emulation, and it is slow.
termux-test:
    docker build --platform linux/amd64 -f Dockerfile.termux -t blivet-termux .
    docker run --rm --platform linux/amd64 --init blivet-termux

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

# Checks that .releaserc.json's plugins still pick the right release type and
# render commits into the notes. A preset/plugin major mismatch otherwise
# publishes with an empty changelog and GitHub release body.
check-release-config:
    node scripts/check-release-config.mjs

# Run semantic-release (used by release workflow)
release:
    npm ci
    just check-release-config
    npx semantic-release
