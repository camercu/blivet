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

# Scratch paths carry the pid, so concurrent runs cannot read each other's
# output and conclude the wrong thing about their own.
scratch="${TMPDIR:-.}/.assert-tests-ran.$$"
log="$scratch.log"
status_file="$scratch.status"
trap 'rm -f "$log" "$status_file"' EXIT INT TERM

# Stream the output and keep the command's own exit status. `$?` after a
# pipeline is the last stage's, and `pipefail` is not POSIX, so the status
# travels through a file.
# `set +e` inside the group: errexit would abandon the subshell the moment the
# command failed, losing the very status this is here to carry.
{ set +e; "$@" 2>&1; echo $? >"$status_file"; } | tee "$log"
status=$(cat "$status_file")

# Sum every suite's count: one invocation prints one "test result:" line per
# suite, and a tier may chain several invocations.
passed=$(awk '/^test result:/ { total += $4 } END { print total + 0 }' "$log")

if [ "$passed" -eq 0 ]; then
    echo "FAIL: the test command reported no tests at all." >&2
    echo "A suite that matches nothing still exits 0, so this is a green run" >&2
    echo "that proved nothing — usually cargo judging stale sources fresh." >&2
    exit 1
fi

exit "$status"
