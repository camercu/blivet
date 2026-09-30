let
  pkgs = import ./nix/pinned.nix;
in
pkgs.mkShell {
  packages = with pkgs; [
    rustup
    just
    jq
    pre-commit
    docker-client
    nodejs
    pandoc
    cargo-deny
    cargo-llvm-cov
    cargo-mutants
    cargo-nextest
    cargo-public-api
  ];
}
