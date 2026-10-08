# Mutation-tests the crate inside the privileged container, as root, with the
# ignored tests included, so a mutant in code only root can exercise meets the
# root tests that kill it. On an unprivileged host those tests skip and every
# such mutant survives.
#
# `--in-diff`: read the diff from stdin and test only the mutants in it. The
# container has no `.git`, so `just mutants-docker` computes the diff on the
# host. Without it, sweep the whole crate.
#
# Invoked as `sh scripts/privileged-mutants.sh [--in-diff]`.
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "FAIL: privileged mutants must run as root; this is uid $(id -u)." >&2
    echo "Root tests skip when they cannot switch user, so a non-root run" >&2
    echo "reports every root-only mutant as missed." >&2
    exit 1
fi

# Appended to the profile in .cargo/mutants.toml: the ignored corpus, as
# privileged-test.sh asks nextest for it.
ignored="--cargo-test-arg=--run-ignored --cargo-test-arg=all"

if [ "${1:-}" = "--in-diff" ]; then
    diff=$(mktemp)
    cat > "$diff"
    exec sh scripts/cargo-mutants.sh $ignored --in-diff "$diff"
fi
exec sh scripts/cargo-mutants.sh $ignored
