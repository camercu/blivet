# Set up development environment (pre-commit hooks, node deps)
setup:
    ./scripts/setup-dev.sh

# Build all targets including tests
build:
    cargo build --tests

# Check formatting
fmt-check:
    cargo fmt --check

# Run clippy lints
lint:
    cargo clippy -- -D warnings

# Build documentation (warnings are errors)
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# Run all static checks
check: fmt-check lint doc

# Run tests (excludes ignored root/Linux tests)
test:
    RUSTFLAGS="-D warnings" cargo test

# Build and run Docker container for root + Linux-specific tests
docker-test:
    docker build -t daemonize-rs-test .
    docker run --rm --init --privileged daemonize-rs-test

# Run everything CI runs (except Docker)
ci: check test

# Run the full CI suite including Docker tests
ci-full: check test docker-test

# Run semantic-release (used by release workflow)
release:
    npm ci
    npx semantic-release
