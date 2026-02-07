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

      runtimeLibs = with pkgs; [
        libxkbcommon
        vulkan-loader
        wayland
      ];

      commonNativeBuildInputs = with pkgs; [
        pkg-config
      ];

      commonBuildInputs = with pkgs; [
        wayland-protocols
        wayland-scanner
      ];

      rust-toolchain = pkgs.rust-bin.nightly.latest.default.override {
        extensions = [
          "rust-src"
        ];
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

      env = {
        RUSTFLAGS = toString [
          "-C link-arg=-Wl,-rpath,${lib.makeLibraryPath runtimeLibs}"
          "-C panic=abort"
        ];
        CARGO_BUILD_TARGET = "x86_64-unknown-linux-gnu";
        CARGO_UNSTABLE_BUILD_STD = "core,alloc,panic_abort";
      };

      Cargo.toml = (fromTOML (builtins.readFile ./Cargo.toml)).package;

      waygaps = rustPlatform.buildRustPackage {
        pname = Cargo.toml.name;
        inherit (Cargo.toml) version;
        inherit env;

        strictDeps = true;

        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./src
            ./build.rs
            ./Cargo.toml
            ./Cargo.lock
            ./protocols
          ];
        };

        doCheck = false;

        cargoLock = {
          lockFile = ./Cargo.lock;
        };

        nativeBuildInputs = commonNativeBuildInputs ++ [ pkgs.makeWrapper ];

        buildInputs = commonBuildInputs;

        postInstall = ''
          wrapProgram $out/bin/waygaps \
          --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
        '';

      };

    in
    {
      devShells.${system}.default = pkgs.mkShell {
        inherit env;

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
