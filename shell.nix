{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  buildInputs = [
    pkgs.rustup
    pkgs.just
    pkgs.docker-client
    pkgs.pre-commit
    pkgs.nodejs
  ];
}
