{
  description = "robolibs crate development shell";

  inputs = {
    # Pinned to a rev that still accepts the `kernel` arg in
    # nvidia-x11/generic.nix. Newer nixpkgs (post 2026-04) dropped
    # that arg, which breaks nixGL until upstream catches up. Bump
    # together with nixgl when its corresponding fix lands.
    nixpkgs.url = "github:NixOS/nixpkgs?rev=4c1018dae018162ec878d42fec712642d214fdfa";
    rust-overlay.url = "github:oxalica/rust-overlay?rev=3c27f4c92a7d977556dd2c10bb564d9c61b375e9";
    flake-utils.url = "github:numtide/flake-utils";
    nixgl.url = "github:nix-community/nixGL";
  };

  outputs =
    { nixpkgs, rust-overlay, flake-utils, nixgl, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [
          (final: prev: {
            xorg = prev.xorg // {
              libX11 = final.libx11;
              libxcb = final.libxcb;
              libxshmfence = final.libxshmfence;
            };
          })
          (import rust-overlay)
        ];

        pkgs = import nixpkgs {
          inherit system overlays;
          config = {
            allowUnfree = true;
            nvidia.acceptLicense = true;
          };
        };

        nvidiaVersion = builtins.getEnv "NVIDIA_VERSION";
        hasNvidia = nvidiaVersion != "";

        nixglPkgs = import "${nixgl}/default.nix" ({
          inherit pkgs;
        } // pkgs.lib.optionalAttrs hasNvidia {
          inherit nvidiaVersion;
          nvidiaHash = null;
        });

        nixGLTarget =
          if hasNvidia
          then "${nixglPkgs.nixGLNvidia}/bin/nixGLNvidia-${nvidiaVersion}"
          else "${nixglPkgs.nixGLIntel}/bin/nixGLIntel";
        nixVulkanTarget =
          if hasNvidia
          then "${nixglPkgs.nixVulkanNvidia}/bin/nixVulkanNvidia-${nvidiaVersion}"
          else "${nixglPkgs.nixVulkanIntel}/bin/nixVulkanIntel";

        nixGLAlias = pkgs.runCommand "nixGL" { } ''
          mkdir -p $out/bin
          ln -s ${nixGLTarget} $out/bin/nixGL
        '';
        nixVulkanAlias = pkgs.runCommand "nixVulkan" { } ''
          mkdir -p $out/bin
          ln -s ${nixVulkanTarget} $out/bin/nixVulkan
        '';

        # cargo-fuzz drives rustc with `-Z sanitizer=address`, which only
        # nightly accepts. Keeping nightly out of the default shell means the
        # everyday toolchain stays pinned to stable — the fuzzers get their own
        # shell instead (`nix develop .#fuzz`, or `make fuzz`).
        rustNightly = pkgs.rust-bin.nightly.latest.default.override {
          extensions = [ "rust-src" "llvm-tools-preview" ];
        };

        guiLibs = with pkgs; [
          alsa-lib
          udev
          vulkan-loader
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            (pkgs.rust-bin.stable.latest.default.override {
              extensions = [ "rust-src" "rustfmt" "clippy" ];
              targets = [ "wasm32-unknown-unknown" ];
            })
            pkgs.clang
            pkgs.mold
            pkgs.pkg-config
            pkgs.rust-cbindgen
            pkgs.trunk
            pkgs.maturin
            pkgs.typst
            (pkgs.python3.withPackages (ps: with ps; [ fonttools brotli pip ]))

            nixGLAlias
            nixVulkanAlias
            nixglPkgs.nixGLIntel
            nixglPkgs.nixVulkanIntel
          ] ++ pkgs.lib.optionals hasNvidia [
            nixglPkgs.nixGLNvidia
            nixglPkgs.nixVulkanNvidia
          ] ++ guiLibs;

          RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath guiLibs;
          WGPU_VALIDATION = "0";
          WGPU_DEBUG = "0";
        };

        # Fuzzing shell: nightly plus cargo-fuzz and nothing else it does not
        # need. `cd fuzz && cargo fuzz run <target>`, or `make fuzz` from the
        # repository root.
        devShells.fuzz = pkgs.mkShell {
          packages = [
            rustNightly
            pkgs.cargo-fuzz
            pkgs.clang
            pkgs.mold
            pkgs.pkg-config
          ];
          RUST_SRC_PATH = "${rustNightly}/lib/rustlib/src/rust/library";
        };
      }
    );
}
