# Runs the privileged tier's suite: every test, ignored included, as root.
#
# The tier's claim is not "tests ran" — `assert-tests-ran.sh` answers that —
# but "the privileged tests ran, as root". Both preconditions used to fail
# silently:
#
#   * run unprivileged, and the eleven root tests skip politely, report
#     "12 passed", and exit 0 having exercised no privileged behaviour;
#   * drop the flag that asks for ignored tests, and the whole root suite
#     reports "0 passed; 0 failed; 12 ignored" while the other suites' counts
#     satisfy the guard.
#
# Both are asserted here, in the one place the Dockerfile names, so the flags
# and the preconditions cannot drift apart across two spellings.
#
# Invoked as `sh scripts/privileged-test.sh`: same reason the guard is.
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "FAIL: the privileged tier must run as root; this is uid $(id -u)." >&2
    echo "Its privileged tests skip when they cannot switch user, so a" >&2
    echo "non-root run reports passes having proven nothing." >&2
    exit 1
fi

scratch=$(mktemp -d 2>/dev/null || true)
if [ -z "$scratch" ]; then
    scratch="${TMPDIR:-/tmp}/privileged-test.$$"
    mkdir -p "$scratch"
fi
trap 'rm -rf "$scratch"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
log="$scratch/output"

# --run-ignored all: the ignored corpus is this tier's whole reason to exist.
# Every test the range moved from an environment-variable skip to `#[ignore]`
# runs here and nowhere else.
sh scripts/assert-tests-ran.sh \
    cargo nextest run --profile privileged --run-ignored all --locked \
    >"$log" 2>&1 || {
    cat "$log"
    exit 1
}
cat "$log"

# Nothing may be filtered out. With no filter and every ignored test requested,
# a non-zero skip count means the run did not ask for what this tier claims to
# prove — which is the failure above, seen from the summary rather than guessed
# at from the flags.
skipped=$(awk '
    { gsub(/\033\[[0-9;]*m/, "") }
    /^ *Summary \[/ { for (i = 1; i <= NF; i++) if ($i == "skipped") print $(i - 1) }
' "$log")
case "$skipped" in
    '' | *[!0-9]*)
        echo "FAIL: could not read the skip count from the run." >&2
        exit 1
        ;;
    0) ;;
    *)
        echo "FAIL: $skipped test(s) were skipped in the privileged tier." >&2
        echo "It runs with every ignored test requested, so nothing should be" >&2
        echo "filtered out; a skip here is coverage the tier claims and lacks." >&2
        exit 1
        ;;
esac

# Doctests are a separate harness: nextest does not run them, and rustdoc maps
# an `ignore` code block to a libtest-ignored test, so asking for ignored ones
# would try to compile README fragments marked `ignore` precisely because they
# cannot compile.
sh scripts/assert-tests-ran.sh cargo test --locked --doc
