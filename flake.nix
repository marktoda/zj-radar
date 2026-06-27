{
  description = "zj-radar — Zellij sidebar plugin for AI-agent status";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, fenix, crane, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        fx = fenix.packages.${system};
        # Host toolchain + the wasm32-wasip1 std the Zellij plugin compiles against.
        toolchain = fx.combine [
          fx.stable.toolchain
          fx.targets.wasm32-wasip1.stable.rust-std
        ];
        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

        src = craneLib.cleanCargoSource ./.;
        commonArgs = {
          inherit src;
          strictDeps = true;
          # zellij-tile's dependency tree pulls openssl-sys (via isahc → curl);
          # its build script needs openssl + pkg-config present. Shared via
          # commonArgs so both the host checks (test/clippy) and the wasm dep
          # build can compile it.
          buildInputs = [ pkgs.openssl ];
          nativeBuildInputs = [ pkgs.pkg-config ];
        };

        # ── wasm plugin artifact (cross-compiled to wasm32-wasip1) ──
        wasmArgs = commonArgs // {
          CARGO_BUILD_TARGET = "wasm32-wasip1";
          doCheck = false; # wasm can't execute on the host builder; see `checks` for tests
        };
        cargoArtifactsWasm = craneLib.buildDepsOnly wasmArgs;
        zj-radar = craneLib.buildPackage (wasmArgs // {
          cargoArtifacts = cargoArtifactsWasm;
          doInstallCargoArtifacts = false;
          # The wasm bin isn't an ELF executable; install it by hand to $out/lib.
          installPhaseCommand = ''
            mkdir -p $out/lib
            cp target/wasm32-wasip1/release/zj_radar.wasm $out/lib/zj_radar.wasm
          '';
        });

        # ── host-target deps shared by the test/clippy checks ──
        cargoArtifactsHost = craneLib.buildDepsOnly commonArgs;
      in {
        packages.default = zj-radar;
        packages.zj-radar = zj-radar;

        checks = {
          inherit zj-radar;
          clippy = craneLib.cargoClippy (commonArgs // {
            cargoArtifacts = cargoArtifactsHost;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          test = craneLib.cargoTest (commonArgs // {
            cargoArtifacts = cargoArtifactsHost;
          });
        };

        devShells.default = pkgs.mkShell {
          packages = [ toolchain pkgs.zellij ];
          shellHook = ''
            echo "zj-radar dev shell: $(rustc --version)"
            echo "build:  cargo build --release --target wasm32-wasip1"
            echo "test:   cargo test"
          '';
        };
      });
}
