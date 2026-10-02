# Publishes blivet <version> to crates.io, unless crates.io already has it.
#
# Runs in semantic-release's prepare step, before @semantic-release/git
# commits and pushes the release commit: a failed publish then leaves no
# release commit and no tag, and a re-run tries again. The skip covers the
# opposite failure, a publish that succeeded before the release commit's push
# failed: HEAD is still the commit the crate was built from, and the re-run
# goes on without publishing the version twice, which crates.io would refuse.
# It skips only when the published crate was built from this very commit (its
# .cargo_vcs_info.json names it); otherwise the release stops for a human.
#
# A failed tag push is not covered. The release commit is on main by then,
# so a re-run would see HEAD one commit past the published crate and stop
# here (and would add the release notes to CHANGELOG.md a second time if it
# went on). Push the tag by hand instead:
#   git tag v<version> <release commit> && git push origin v<version>
#
# Invoked as `sh scripts/publish-crate.sh <version>`.
set -eu
version=${1:?usage: publish-crate.sh <version>}
agent='blivet-release (https://github.com/camercu/blivet)'
status=$(curl -sS -o /dev/null -w '%{http_code}' -A "$agent" \
    "https://crates.io/api/v1/crates/blivet/$version")
case $status in
    200)
        published=$(curl -sSfL -A "$agent" \
            "https://crates.io/api/v1/crates/blivet/$version/download" \
            | tar xzOf - "blivet-$version/.cargo_vcs_info.json" \
            | sed -n 's/.*"sha1": *"\([0-9a-f]*\)".*/\1/p')
        head=$(git rev-parse HEAD)
        if [ "$published" != "$head" ]; then
            echo "blivet $version on crates.io was built from ${published:-an unknown commit}, not HEAD $head; not tagging a different tree" >&2
            exit 1
        fi
        echo "blivet $version is already on crates.io, built from HEAD; not publishing it again"
        ;;
    404)
        # Cargo.toml now holds the version semantic-release chose. Refuse to
        # publish an API change that version does not allow: a breaking change
        # committed without `!` would otherwise ship as a minor release.
        nix-shell nix/semver-checks.nix --run 'just semver-checks-toolchain semver-checks'
        # --allow-dirty: prepare has just rewritten Cargo.toml, Cargo.lock
        # and CHANGELOG.md, which @semantic-release/git commits after this.
        cargo publish --locked --allow-dirty
        ;;
    *)
        echo "crates.io answered HTTP $status for blivet $version" >&2
        exit 1
        ;;
esac
