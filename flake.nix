{
  description = "Tapview - touchpad heatmap visualizer";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        runtimeLibs = with pkgs; [
          libGL
          libx11
          libxcursor
          libxrandr
          libxi
          libxcb
          libxkbcommon
          vulkan-loader
          wayland
        ];
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "tapview";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
            makeWrapper
          ];

          buildInputs = with pkgs; [
            systemd
            libinput
          ] ++ runtimeLibs;

          postInstall = ''
            wrapProgram $out/bin/tapview \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibs}
          '';
        };

        devShells.default =
          with pkgs;
          mkShell rec {
            buildInputs = [
              pkg-config
              rust-bin.nightly.latest.default
              systemd
              libinput
            ] ++ runtimeLibs;

            shellHook = ''
              export LD_LIBRARY_PATH="$LD_LIBRARY_PATH:${builtins.toString (pkgs.lib.makeLibraryPath buildInputs)}";
            '';
          };
      }
    );
}
