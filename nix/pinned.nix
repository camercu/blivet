# The one nixpkgs pin every shell in this repo builds from, so the dev shell,
# CI and the release job run the same tool versions.
import (builtins.fetchTarball {
  url = "https://github.com/NixOS/nixpkgs/archive/ed142ab1b3a092c4d149245d0c4126a5d7ea00b0.tar.gz";
  sha256 = "1h7v295lpjfxpxkag2csam7whx918sdypixdi8i85vlb707gg0vm";
}) {}
