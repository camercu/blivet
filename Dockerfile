FROM rust:1.87-slim-bookworm

# Install tools used by integration test helpers
RUN apt-get update && apt-get install -y --no-install-recommends \
    lsof procps \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user and extra group for user/group-switching tests
RUN useradd --create-home --shell /bin/bash testuser \
    && groupadd testgroup

WORKDIR /src

# Cache dependencies by copying manifests first
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo '' > src/lib.rs && echo 'fn main() {}' > src/main.rs \
    && cargo build --locked --tests 2>/dev/null || true \
    && rm -rf src

# Copy full source
COPY . .

# Build tests (this layer is cached as long as source doesn't change)
RUN cargo build --locked --tests

# Run all tests including root-only and Linux-specific. Doctests run
# separately without --include-ignored: rustdoc maps `ignore` code blocks to
# libtest-ignored tests, so --include-ignored would try to compile README
# fragments that are marked `ignore` precisely because they cannot compile.
CMD ["sh", "-c", "cargo test --locked --all-targets -- --include-ignored && cargo test --locked --doc"]
