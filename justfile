# Build all targets including tests
build:
    cargo build --tests

# Run tests (excludes ignored root/Linux tests)
test:
    cargo test

# Run clippy, fmt check, and doc build
check:
    cargo clippy -- -D warnings
    cargo fmt --check
    cargo doc --no-deps

# Build and run Docker container for root + Linux-specific tests
docker-test:
    docker build -t daemonize-rs-test .
    docker run --rm --init --privileged daemonize-rs-test

# Run everything CI runs (except Docker)
ci: check test

# Run the full CI suite including Docker tests
ci-full: check test docker-test
