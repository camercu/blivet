# Environment variable: set CARGO_LOCKED=--locked in CI for reproducibility
locked := env("CARGO_LOCKED", "")

# Set up development environment (pre-commit hooks, node deps)
setup:
    ./scripts/setup-dev.sh

# Build all targets including tests
build:
    cargo build {{locked}} --tests

# Check formatting
fmt-check:
    cargo fmt --check

# Run clippy lints
lint:
    cargo clippy {{locked}} -- -D warnings

# Run cargo-deny checks (advisories, licenses, bans)
lint-deny:
    cargo deny check

# Build documentation (warnings are errors)
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc {{locked}} --no-deps

# Run all static checks
check: fmt-check lint lint-deny doc

# Run tests (excludes ignored root/Linux tests)
test:
    RUSTFLAGS="-D warnings" cargo test {{locked}}

# Build and run Docker container for root + Linux-specific tests
docker-test:
    docker build -t blivet-test .
    docker run --rm --init --privileged blivet-test

# Run everything CI runs (except Docker)
ci: check test

# Run the full CI suite including Docker tests
ci-full: check test docker-test

# Run semantic-release (used by release workflow)
release:
    npm ci
    npx semantic-release
