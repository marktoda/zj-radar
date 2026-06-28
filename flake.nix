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
        # Minimal host toolchain + the wasm32-wasip1 std the Zellij plugin
        # compiles against. Avoid fx.stable.toolchain here: it pulls extra
        # rust-docs/rust-src/rust-analyzer components into the dev hot path.
        toolchain = fx.combine [
          fx.stable.cargo
          fx.stable.clippy
          fx.stable.rustc
          fx.stable.rustfmt
          fx.targets.wasm32-wasip1.stable.rust-std
        ];
        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (craneLib.filterCargoSources path type)
            || (pkgs.lib.hasSuffix "/examples/radar-template-snippet.kdl" path);
        };
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
          # Install the wasm to $out/bin to match the Zellij-plugin convention
          # (e.g. zjstatus → ${pkgs.zjstatus}/bin/zjstatus.wasm), so downstream
          # layouts reference ${pkg}/bin/zj_radar.wasm like every other plugin.
          installPhaseCommand = ''
            mkdir -p $out/bin
            cp target/wasm32-wasip1/release/zj_radar.wasm $out/bin/zj_radar.wasm
          '';
        });

        # ── host-target deps shared by the test/clippy checks ──
        cargoArtifactsHost = craneLib.buildDepsOnly commonArgs;

        # ── native CLI (host target, `cli` feature) ──
        cliArgs = commonArgs // { cargoExtraArgs = "--features cli"; };
        cargoArtifactsCli = craneLib.buildDepsOnly cliArgs;
        zj-radar-cli = craneLib.buildPackage (cliArgs // {
          pname = "zj-radar-cli";
          cargoArtifacts = cargoArtifactsCli;
          cargoExtraArgs = "--features cli --bin zj-radar";
          doCheck = false;
        });
      in {
        packages.default = zj-radar;
        packages.zj-radar = zj-radar;
        packages.zj-radar-cli = zj-radar-cli;

        checks = {
          inherit zj-radar;
          clippy = craneLib.cargoClippy (commonArgs // {
            cargoArtifacts = cargoArtifactsHost;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          test = craneLib.cargoTest (commonArgs // {
            cargoArtifacts = cargoArtifactsHost;
          });
          cli-test = craneLib.cargoTest (cliArgs // {
            cargoArtifacts = cargoArtifactsCli;
          });
          cli-clippy = craneLib.cargoClippy (cliArgs // {
            cargoArtifacts = cargoArtifactsCli;
            cargoClippyExtraArgs = "--all-targets --features cli -- -D warnings";
          });
        };

        devShells.default = pkgs.mkShell {
          packages = [ toolchain pkgs.jq pkgs.zellij ];
          shellHook = ''
            echo "zj-radar dev shell: $(rustc --version)"
            echo "dev:    ./dev/run.sh"
            echo "build:  ./dev/run.sh --build-only"
            echo "test:   cargo test"
          '';
        };
      });
}
