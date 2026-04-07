FROM rust:1.87-slim-bookworm

# Install tools used by integration test helpers
RUN apt-get update && apt-get install -y --no-install-recommends \
    lsof procps \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user for user-switching tests
RUN useradd --create-home --shell /bin/bash testuser

WORKDIR /src

# Cache dependencies by copying manifests first
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo '' > src/lib.rs && echo 'fn main() {}' > src/main.rs \
    && cargo build --tests 2>/dev/null || true \
    && rm -rf src

# Copy full source
COPY . .

# Build tests (this layer is cached as long as source doesn't change)
RUN cargo build --tests

# Run all tests including root-only and Linux-specific
CMD ["cargo", "test", "--", "--include-ignored"]
