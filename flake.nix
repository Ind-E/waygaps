{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      ...
    }:
    let
      inherit (nixpkgs) lib;

      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ (import rust-overlay) ];
      };

      commonNativeBuildInputs = with pkgs; [
        pkg-config
      ];

      commonBuildInputs = with pkgs; [
        wayland-protocols
        wayland-scanner
      ];

      rust-toolchain = pkgs.rust-bin.nightly.latest.default.override {
        extensions = [ "rust-src" ];
        targets = [ "x86_64-unknown-linux-gnu" ];
      };

      rust-toolchain-dev = rust-toolchain.override {
        extensions = [
          "rust-src"
          "rust-analyzer"
        ];
      };

      rustPlatform = pkgs.makeRustPlatform {
        rustc = rust-toolchain;
        cargo = rust-toolchain;
      };

      Cargo.toml = (fromTOML (builtins.readFile ./Cargo.toml)).package;

      waygaps = rustPlatform.buildRustPackage {
        pname = Cargo.toml.name;
        inherit (Cargo.toml) version;

        strictDeps = true;

        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./src
            ./build.rs
            ./Cargo.toml
            ./Cargo.lock
            ./.cargo/config.toml
            ./protocols
          ];
        };

        doCheck = false;

        cargoLock = {
          lockFile = ./Cargo.lock;
        };

        nativeBuildInputs = commonNativeBuildInputs;
        buildInputs = commonBuildInputs;

        meta = {
          platforms = [ system ];
        };
      };

    in
    {
      devShells.${system}.default = pkgs.mkShell {
        nativeBuildInputs = commonNativeBuildInputs ++ [ rust-toolchain-dev ];
        buildInputs = commonBuildInputs;
      };

      packages.${system} = {
        inherit waygaps;
        default = waygaps;
      };

      overlays.default = final: _: {
        inherit waygaps;
      };
    };
}
