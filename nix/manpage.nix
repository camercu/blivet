# Just what `just manpage` needs. The release job builds the man page with
# this rather than the dev shell: every package a shell pulls from the binary
# cache is one more download that can fail and stop a release, and the dev
# shell pulls hundreds. Same pin as shell.nix, so the page matches.
let
  pkgs = import ./pinned.nix;
in
pkgs.mkShell {
  packages = with pkgs; [
    just
    pandoc
  ];
}
