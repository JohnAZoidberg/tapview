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

        # Rust toolchain with Windows cross-compilation target
        rustToolchainWindows = pkgs.rust-bin.stable.latest.default.override {
          targets = [ "x86_64-pc-windows-gnu" ];
        };

        # MinGW cross-compiler toolchain
        mingw = pkgs.pkgsCross.mingwW64.stdenv.cc;
        mingwPthreads = pkgs.pkgsCross.mingwW64.windows.pthreads;
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

        packages.windows =
          let
            cargoVendorDir = pkgs.rustPlatform.importCargoLock {
              lockFile = ./Cargo.lock;
            };
          in
          pkgs.stdenv.mkDerivation {
            pname = "tapview-windows";
            version = "0.1.0";
            src = ./.;

            nativeBuildInputs = [
              rustToolchainWindows
              mingw
            ];

            buildPhase = ''
              export HOME=$(mktemp -d)

              # Vendor dependencies (no network in nix build)
              ln -s ${cargoVendorDir} vendor
              mkdir -p .cargo
              cat > .cargo/config.toml <<TOML
              [source.crates-io]
              replace-with = "vendored-sources"

              [source.vendored-sources]
              directory = "vendor"
              TOML

              export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="${mingw}/bin/x86_64-w64-mingw32-gcc"
              export CC_x86_64_pc_windows_gnu="${mingw}/bin/x86_64-w64-mingw32-gcc"
              export CXX_x86_64_pc_windows_gnu="${mingw}/bin/x86_64-w64-mingw32-g++"
              export AR_x86_64_pc_windows_gnu="${mingw}/bin/x86_64-w64-mingw32-ar"
              export RUSTFLAGS="-L ${mingwPthreads}/lib"
              cargo build --release --target x86_64-pc-windows-gnu
            '';

            installPhase = ''
              mkdir -p $out/bin
              cp target/x86_64-pc-windows-gnu/release/tapview.exe $out/bin/
            '';
          };

        devShells.default =
          with pkgs;
          mkShell rec {
            buildInputs = [
              pkg-config
              rust-bin.stable.latest.default
              systemd
              libinput
            ] ++ runtimeLibs;

            shellHook = ''
              export LD_LIBRARY_PATH="$LD_LIBRARY_PATH:${builtins.toString (pkgs.lib.makeLibraryPath buildInputs)}";
            '';
          };

        devShells.cross-windows =
          pkgs.mkShell {
            buildInputs = [
              rustToolchainWindows
              mingw
            ];

            shellHook = ''
              export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="${mingw}/bin/x86_64-w64-mingw32-gcc"
              export CC_x86_64_pc_windows_gnu="${mingw}/bin/x86_64-w64-mingw32-gcc"
              export CXX_x86_64_pc_windows_gnu="${mingw}/bin/x86_64-w64-mingw32-g++"
              export AR_x86_64_pc_windows_gnu="${mingw}/bin/x86_64-w64-mingw32-ar"
              export RUSTFLAGS="-L ${mingwPthreads}/lib"
              echo "Cross-compilation shell ready. Build with:"
              echo "  cargo build --release --target x86_64-pc-windows-gnu"
            '';
          };
      }
    );
}
