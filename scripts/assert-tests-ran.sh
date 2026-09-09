# Runs a test command and fails if it reported no tests.
#
# `cargo test` prints "test result: ok." and exits 0 when its filter matches
# nothing, so a container whose sources look stale to cargo builds green having
# proven nothing. Every container tier runs its suite through here, so that
# outcome is a red job rather than a silent one.
#
# Invoked as `sh scripts/assert-tests-ran.sh <command>...`: Termux has no
# /bin/sh for a shebang to name.
set -eu

# A directory of its own, so concurrent runs cannot read each other's output
# and conclude the wrong thing about their own. `mktemp -d` honours TMPDIR and
# is what keeps a transcript of the whole run out of the working tree: a guard
# whose subject is build hygiene must not leave files among the sources it is
# about to scan, and TMPDIR is unset on a GitHub Linux runner. The fallback is
# for a system without mktemp; Termux has no /tmp but does set TMPDIR.
scratch=$(mktemp -d 2>/dev/null || true)
if [ -z "$scratch" ]; then
    scratch="${TMPDIR:-/tmp}/assert-tests-ran.$$"
    mkdir -p "$scratch"
fi
log="$scratch/output"
status_file="$scratch/status"
trap 'rm -rf "$scratch"' EXIT INT TERM

# Stream the output and keep the command's own exit status. `$?` after a
# pipeline is the last stage's, and `pipefail` is not POSIX, so the status
# travels through a file.
# `set +e` inside the group: errexit would abandon the subshell the moment the
# command failed, losing the very status this is here to carry.
{ set +e; "$@" 2>&1; echo $? >"$status_file"; } | tee "$log"
status=$(cat "$status_file")

# Sum every suite's counts: one invocation prints one "test result:" line per
# suite, and a tier may chain several invocations.
#
# Summed across suites rather than checked per suite, which bounds what this
# can prove: where a tier runs several suites, one that matched nothing is
# hidden by any other that ran. Per-suite checking is not available — a run of
# `--all-targets` legitimately includes suites with no tests, so an empty one
# is not by itself wrong. What the guard catches is the whole tree going stale,
# which zeroes every suite at once; the stale-fingerprint deletion in each
# Dockerfile is what stops one binary going stale on its own.
#
# Passed AND failed, because
# the question here is whether anything ran at all — a suite where every test
# failed has a passed count of zero and must keep its own status and its own
# diagnosis rather than being blamed on a stale build.
#   test result: FAILED. 0 passed; 5 failed; ...
#                        ^$4            ^$6
ran=$(awk '/^test result:/ { total += $4 + $6 } END { print total + 0 }' "$log")

# Only a *successful* command that ran nothing is the case this guard is for.
# A command that failed has already said why — a compile error, a missing
# binary — and blaming stale sources for it sends the reader somewhere else.
if [ "$status" -eq 0 ] && [ "$ran" -eq 0 ]; then
    echo "FAIL: the test command reported no tests at all." >&2
    echo "A suite that matches nothing still exits 0, so this is a green run" >&2
    echo "that proved nothing — usually cargo judging stale sources fresh." >&2
    exit 1
fi

exit "$status"
