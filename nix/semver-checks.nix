# Just what `just semver-checks` needs. The release job runs it from this
# small shell rather than the dev shell, for the reason nix/manpage.nix gives:
# every package pulled from the binary cache is one more download that can
# fail and stop a release. Same pin as shell.nix.
let
  pkgs = import ./pinned.nix;
in
pkgs.mkShell {
  packages = with pkgs; [
    just
    cargo-semver-checks
  ];
}
