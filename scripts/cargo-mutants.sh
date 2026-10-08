# cargo-mutants, with a timed-out mutant counted as caught: its tests did not
# pass. Exit 3 cannot say that alone, because cargo-mutants returns it whenever
# any mutant timed out, ahead of the 2 for a missed one; so 3 passes only when
# `mutants.out/missed.txt` exists and is empty. An earlier run's file is removed
# first, so a run that writes elsewhere (`-o DIR`) finds none and 3 fails.
#
# One script for both places mutants run: the host (`just mutants`,
# `just mutants-diff`) and the privileged container (privileged-mutants.sh),
# so the two cannot count a mutant differently.
#
# Invoked as `sh scripts/cargo-mutants.sh ARGS...`.
set -u

rm -f mutants.out/missed.txt
cargo mutants ${CARGO_LOCKED:+--cargo-arg=$CARGO_LOCKED} "$@" && exit 0
rc=$?
if [ "$rc" -eq 3 ] && [ -f mutants.out/missed.txt ] && [ ! -s mutants.out/missed.txt ]; then
    echo "cargo-mutants: some mutants timed out and none were missed; a timeout counts as caught" >&2
    exit 0
fi
exit "$rc"
