# Publishes blivet <version> to crates.io, unless crates.io already has it.
#
# Runs in semantic-release's prepare step, before @semantic-release/git
# commits and tags: a failed publish then leaves no release commit and no tag,
# and a re-run tries again. The skip covers the opposite failure, a publish
# that succeeded before the tag push failed: the re-run tags without trying
# to publish the version twice, which crates.io would refuse.
#
# Invoked as `sh scripts/publish-crate.sh <version>`.
set -eu
version=${1:?usage: publish-crate.sh <version>}
status=$(curl -sS -o /dev/null -w '%{http_code}' \
    -A 'blivet-release (https://github.com/camercu/blivet)' \
    "https://crates.io/api/v1/crates/blivet/$version")
case $status in
    200)
        echo "blivet $version is already on crates.io; not publishing it again"
        ;;
    404)
        # --allow-dirty: prepare has just rewritten Cargo.toml, Cargo.lock
        # and CHANGELOG.md, which @semantic-release/git commits after this.
        cargo publish --locked --allow-dirty
        ;;
    *)
        echo "crates.io answered HTTP $status for blivet $version" >&2
        exit 1
        ;;
esac
