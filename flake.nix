{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
      ...
    }:
    let
      inherit (nixpkgs.lib) genAttrs getExe fileset;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      forEachSystem =
        fn:
        genAttrs systems (
          system:
          fn {
            pkgs = import nixpkgs {
              inherit system;
              overlays = [ (import rust-overlay) ];
            };
            inherit system;
          }
        );
    in
    {
      packages = forEachSystem (
        { pkgs, ... }:
        let
          rustToolchain = pkgs.rust-bin.nightly.latest.default;
          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          commonArgs = {
            src = fileset.toSource {
              root = ./.;
              fileset = fileset.unions [
                ./src
                ./build.rs
                ./Cargo.toml
                ./Cargo.lock
                ./protocols
                ./example-config.toml
              ];
            };
            strictDeps = true;
            nativeBuildInputs = with pkgs; [
              pkg-config
            ];

            buildInputs = with pkgs; [
              wayland-scanner
              wayland-protocols
            ];

          };
        in
        {
          default = craneLib.buildPackage (
            commonArgs
            // {
              cargoArtifacts = craneLib.buildDepsOnly commonArgs;
              doCheck = false;
              meta = {
                mainProgram = "waygaps";
              };
            }
          );
        }
      );

      apps = forEachSystem (
        { system, ... }: {
          default = {
            type = "app";
            program = getExe self.packages.${system}.default;
          };
        }
      );

      checks = forEachSystem (
        { system, ... }: {
          inherit (self.packages.${system}) default;
        }
      );

      devShells = forEachSystem (
        { pkgs, system }:
        let
          rustDevToolchain = pkgs.rust-bin.nightly.latest.default.override {
            extensions = [
              "rust-src"
              "rust-analyzer"
            ];
          };
          craneLib = (crane.mkLib pkgs).overrideToolchain rustDevToolchain;
        in
        {
          default = craneLib.devShell {
            checks = self.checks.${system};
          };
        }
      );
    };
}
