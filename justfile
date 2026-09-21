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
#
# "Any other Unix" is not checkable as a class — `x86_64-pc-solaris` does not
# compile, because `nix` gates `Flock` and `sys::resource` away there. illumos
# stands in for the tier: it is a Unix the table does not list, so it exercises
# every fallback branch, and it has a rustup std to check against.
check-cross:
    rustup target add x86_64-unknown-linux-gnu x86_64-unknown-freebsd x86_64-unknown-netbsd aarch64-linux-android x86_64-linux-android x86_64-unknown-illumos
    {{cargo}} check {{locked}} --target x86_64-unknown-linux-gnu
    {{cargo}} check {{locked}} --target x86_64-unknown-freebsd
    {{cargo}} check {{locked}} --target x86_64-unknown-netbsd
    {{cargo}} check {{locked}} --target aarch64-linux-android
    {{cargo}} check {{locked}} --target x86_64-linux-android
    # illumos is not in the capability table: it is the best-effort tier's
    # stand-in, so this is the only gate behind the row promising those targets
    # compile. --all-targets because what breaks there is the shipped example
    # and the test helpers reaching for a capability-gated item, not the
    # library; -D warnings because a deprecated stub is a warning, and the
    # stubs are what a best-effort target resolves to.
    RUSTFLAGS="-D warnings" {{cargo}} check {{locked}} --target x86_64-unknown-illumos --all-targets

# Lowest rustc that must be able to *resolve* the dev-dependency graph: the
# rust OpenBSD ships via `pkg_add rust` (Tier 3, no rustup), which the openbsd
# smoke job runs `cargo test --lib` under. Bump when OpenBSD packages a newer
# rust; that is also what unblocks the serial_test ignore in dependabot.yml.
openbsd_rust := "1.90"

# Lowest rustc that must be able to *compile* the dev-dependency graph: the
# base image of the privileged container tier, which runs
# `cargo build --locked --tests` and so builds every dev-dependency. It is a
# release-gating tier, so a dev-dep landing between this and `openbsd_rust`
# would pass the cheap guard below and break the expensive one. Headroom is
# zero today: heapless declares exactly this. `tests/toolchain_floors.rs` holds
# the Dockerfile's `FROM rust:` to it.
docker_rust := "1.87"

# Fail if a resolved dependency declares a rust-version too high for the
# toolchains CI must satisfy. Two floors, because the failure modes differ:
#
#   runtime deps (normal + build) — must fit this crate's MSRV, or consumers
#       on that MSRV cannot build blivet at all.
#   dev-dependencies — need only fit the oldest rustc that builds or runs our
#       test suite, which is the lower of {{openbsd_rust}} and {{docker_rust}}.
#       Over-declaring is common (heapless declares 1.87 yet compiles on 1.85),
#       so holding dev-deps to the crate MSRV would reject working graphs.
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
    # `rust-version` may be spelled with or without a patch component, and
    # `sort -V` orders 1.85 before 1.85.0 — so a dep that exactly MEETS the
    # floor would be reported as exceeding it. Pad both to three parts first.
    pad() { awk -F. '{ printf "%d.%d.%d\n", $1, ($2 == "" ? 0 : $2), ($3 == "" ? 0 : $3) }' <<<"$1"; }
    # The dev floor is whichever of the two test toolchains is older.
    if [ "$(printf '%s\n%s\n' "$(pad {{openbsd_rust}})" "$(pad {{docker_rust}})" \
        | sort -V | head -1)" = "$(pad {{docker_rust}})" ]; then
        dev_floor={{docker_rust}}
    else
        dev_floor={{openbsd_rust}}
    fi
    runtime=$(cargo tree -e normal,build --prefix none {{locked}} \
        | awk 'NF >= 2 { sub(/^v/, "", $2); print $1 "@" $2 }' | sort -u)
    offenders=$(cargo metadata --format-version 1 {{locked}} \
        --filter-platform "$(rustc -vV | sed -n 's/^host: //p')" \
        | jq -r '.packages[] | select(.rust_version != null) | "\(.name)@\(.version) \(.rust_version)"' \
        | while read -r pkg req; do
            if grep -qxF "$pkg" <<<"$runtime"; then
                floor=$msrv kind=runtime
            else
                floor=$dev_floor kind=dev
            fi
            padded_req=$(pad "$req")
            [ "$(printf '%s\n%s\n' "$(pad "$floor")" "$padded_req" | sort -V | head -1)" = "$padded_req" ] \
                || echo "  $pkg ($kind) requires rustc $req > $floor"
        done)
    if [ -n "$offenders" ]; then
        echo "dependencies exceed the supported rustc floor:"
        echo "$offenders"
        echo "pin the dependency back, or raise the floor deliberately"
        exit 1
    fi
    echo "dependencies fit MSRV $msrv (runtime) and $dev_floor (dev)"

# A non-Unix target must fail with exactly one diagnostic: the `compile_error!`
# in `src/lib.rs` naming the target. Everything else in the crate is gated on
# `cfg(unix)` to keep it that way, and this notices when a new item arrives
# without that gate.
#
# What it catches is name resolution — an ungated `use` or `mod` reaching a
# Unix-only path, which is the wall of `nix` errors this replaced. It cannot
# catch an ungated function *body*: rustc stops after resolution when that
# phase already failed, so a body's type errors are never reached to be
# counted. Parses compiler output, so it stays on plain cargo.
check-non-unix:
    #!/usr/bin/env bash
    set -euo pipefail
    rustup target add wasm32-unknown-unknown
    # --color=never, not merely unset: CI exports CARGO_TERM_COLOR=always, and
    # a coloured diagnostic puts an ANSI reset between "error" and its colon,
    # which no pattern matching plain text will find.
    out=$(cargo check {{locked}} --target wasm32-unknown-unknown \
        --message-format=short --color=never 2>&1 || true)
    # `--message-format=short` writes diagnostics as `file:line:col: error: ...`,
    # so match the marker anywhere; awk always exits 0, unlike a grep that finds
    # nothing. The trailing "could not compile" summary is not a diagnostic.
    errors=$(printf '%s\n' "$out" | awk '/error(\[[A-Z0-9]+\])?: / && !/could not compile/ { n++ } END { print n + 0 }')
    if [ "$errors" -ne 1 ]; then
        echo "expected exactly 1 error on a non-Unix target, got $errors:"
        printf '%s\n' "$out"
        echo "gate the new item on cfg(unix) so the compile_error stands alone"
        exit 1
    fi
    echo "non-Unix target fails with exactly one diagnostic"

# Type-check the library the way a consumer who wants no CLI gets it.
#
# `cli` is on by default, so every other recipe here compiles clap and none of
# them would notice the library itself growing a dependency on it. Without this
# the feature is a claim in the manifest that nothing ever compiles.
#
# --all-targets: the integration tests reach the CLI through Cargo's
# CARGO_BIN_EXE_daemonize, and a binary skipped for unmet required-features is
# the case that would break them.
check-no-default-features:
    RUSTFLAGS="-D warnings" {{cargo}} check --no-default-features --all-targets {{locked}}

# Run all static checks
check: fmt-check lint lint-deny doc msrv-check check-cross check-non-unix check-no-default-features

# Run tests (excludes ignored root/Linux tests)
#
# Under nextest, for the reason `coverage` is: several tests have process-wide
# side effects (redirecting and closing std fds) that clobber the shared
# harness's result pipe, failing the run with a BrokenPipe unrelated to any
# change. nextest gives each test its own process, so none of them can corrupt
# the collector. A gate that fails now and then teaches its readers to re-run
# it, which is how a real red gets waved through.
#
# nextest does not run doctests, so they run after it on the plain harness.
# They are compiled fresh by rustdoc every time and touch no process state, so
# the failure above cannot reach them.
#
# --profile gate, not the default: the default profile belongs to cargo-mutants
# and kills any test at 5s, which killed fourteen CLI integration tests on a
# loaded machine. See .config/nextest.toml.
test:
    RUSTFLAGS="-D warnings" {{cargo}} nextest run --profile gate {{locked}}
    RUSTFLAGS="-D warnings" {{cargo}} test {{locked}} --doc

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
# Under nextest for the same reason `test` is, stated there, and on the same
# profile — through NEXTEST_PROFILE rather than --profile, which cargo-llvm-cov
# reads as a cargo build profile of its own. Left on the default profile this
# recipe inherited cargo-mutants' 5s kill, and instrumented tests are the
# slowest thing here.
coverage:
    NEXTEST_PROFILE=gate cargo llvm-cov nextest --html {{locked}}
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

# Test the crate as a consumer receives it
#
# `src/` ships and the rest of the repository does not, so a `#[cfg(test)]`
# guard in `src/` that reaches outside `Cargo.toml`'s include list passes in a
# checkout and fails for a consumer. That shipped once: a guard read the
# justfile, which is not packaged. `src/doc_sync.rs` bakes its inputs in with
# `include_str!` now, and `tests/packaging.rs` bans repository paths in shipped
# sources — but both of those are checked by compiling the packaged file set,
# which only this recipe does.
package-test:
    #!/usr/bin/env bash
    set -euo pipefail
    root=$PWD
    version=$(cargo metadata --format-version 1 --no-deps \
        | jq -r '.packages[] | select(.name == "blivet") | .version')
    cargo package --locked
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    tar xf "target/package/blivet-$version.crate" -C "$work"
    cd "$work/blivet-$version"
    # Through the guard like every other tier: a packaged copy whose tests all
    # vanished would otherwise report success having proven nothing.
    sh "$root/scripts/assert-tests-ran.sh" cargo test --locked --lib

# Run everything CI runs (except Docker)
ci: check test

# Agent-facing CI: same steps as `ci`, but routes the compile-heavy recipes
# (build/clippy/check/test) through rtk for token-compressed output. Prefer this
# over `ci` when an agent runs the suite. Same pass/fail semantics.
ci-rtk:
    RTK_CARGO="rtk cargo" just ci

# Run the full CI suite including both container tiers
ci-full: check test package-test docker-test termux-test

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
