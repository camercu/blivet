FROM rust:1.87-slim-bookworm

# Install tools used by integration test helpers
RUN apt-get update && apt-get install -y --no-install-recommends \
    lsof procps curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# The tier runs under nextest, which gives each test its own process: the tests
# that close std fds process-wide are exactly the ones this tier exists to run,
# and under a shared harness they corrupt its result pipe.
#
# Pinned, like the nixpkgs revision, the rustdoc nightly and cargo-public-api:
# an image rebuilt months apart otherwise runs a different harness than the one
# a result was recorded against. This is the version shell.nix carries, so a
# developer and the tier agree. Bump both together.
ARG NEXTEST_VERSION=0.9.116
RUN case "$(uname -m)" in \
      x86_64) arch=linux ;; \
      aarch64) arch=linux-arm ;; \
      *) echo "no nextest build for $(uname -m)" >&2; exit 1 ;; \
    esac; \
    curl -LsSf "https://get.nexte.st/${NEXTEST_VERSION}/${arch}" \
      | tar zxf - -C "$CARGO_HOME/bin"

# Create a non-root user and extra group for user/group-switching tests
RUN useradd --create-home --shell /bin/bash testuser \
    && groupadd testgroup

WORKDIR /src

# Cache dependencies by copying manifests first
COPY Cargo.toml Cargo.lock ./
# The stub crate's own fingerprint goes with the stub. Cargo decides a path
# crate is fresh by comparing mtimes, and COPY preserves the build context's,
# which are older than this layer — so leaving the fingerprint behind makes the
# real source look stale and the suite runs zero tests while reporting ok.
RUN mkdir src && echo '' > src/lib.rs && echo 'fn main() {}' > src/main.rs \
    && cargo build --locked --tests 2>/dev/null || true \
    && rm -rf src target/debug/.fingerprint/blivet-*

# Copy full source
COPY . .

# Build tests (this layer is cached as long as source doesn't change)
RUN cargo build --locked --tests

# One name, not a line of flags. The tier's preconditions — root, and every
# ignored test actually requested — are asserted inside it, next to the flags
# that satisfy them, so neither can be dropped while the other stays.
CMD ["sh", "scripts/privileged-test.sh"]
