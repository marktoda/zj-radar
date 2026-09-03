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

  outputs = {
    self,
    nixpkgs,
    fenix,
    crane,
    flake-utils,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {inherit system;};
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
          # Pulled in at compile time via include_str! in src/reference_tests.rs.
          || (pkgs.lib.hasSuffix "/docs/rail-reference.md" path)
          # include_str!'d by the CLI's `run` command (crates/cli/src/run.rs).
          || (pkgs.lib.hasInfix "/crates/cli/src/run_assets/" path)
          # include_str!'d by the example-layout guard test (crates/cli/src/layout.rs).
          || (pkgs.lib.hasSuffix "/examples/radar-sidebar.kdl" path)
          # include_str!'d by the plugin (config.rs, control.rs docs guard tests).
          || (pkgs.lib.hasSuffix "/docs/configuration.md" path)
          # include_str!'d by the CLI's opencode setup (crates/cli/src/setup/mod.rs)
          # — the vendored JS bridge plugin written by `zj-radar setup opencode`.
          || (pkgs.lib.hasSuffix "/crates/cli/src/setup/opencode_plugin.js" path)
          # include_str!'d by the plugin's producer-script guard test (lib.rs).
          || (pkgs.lib.hasSuffix "/plugins/zj-radar-claude/scripts/notify.sh" path)
          # include_str!'d by the plugin's hook-headroom guard (hooks_manifest_tests.rs).
          || (pkgs.lib.hasSuffix "/plugins/zj-radar-claude/hooks/hooks.json" path)
          # include_str!'d by the version-triple weld (hooks_manifest_tests.rs).
          || (pkgs.lib.hasSuffix "/plugins/zj-radar-claude/.claude-plugin/plugin.json" path)
          # Recorded proptest failure seeds: without them the hermetic checks
          # would skip replaying known-bad cases instead of pinning the fixes.
          || (pkgs.lib.hasInfix "/proptest-regressions/" path)
          # insta snapshots (crates/*/src/**/snapshots/*.snap): filterCargoSources
          # keeps only Rust/cargo files, and without the recorded snapshots every
          # insta test "re-records" and fails in the sandbox.
          || (pkgs.lib.hasSuffix ".snap" path);
      };
      commonArgs = {
        inherit src;
        strictDeps = true;
        # zellij-tile's dependency tree pulls openssl-sys (via isahc → curl);
        # its build script needs openssl + pkg-config present. Shared via
        # commonArgs so both the host checks (test/clippy) and the wasm dep
        # build can compile it.
        buildInputs = [pkgs.openssl];
        nativeBuildInputs = [pkgs.pkg-config];
      };

      # ── wasm plugin artifact (the `zj-radar-plugin` member, → wasm32-wasip1) ──
      wasmArgs =
        commonArgs
        // {
          CARGO_BUILD_TARGET = "wasm32-wasip1";
          # Build only the plugin member: the `zj-radar` CLI crate is a host
          # binary (clap/dirs deps) that can't target wasm.
          cargoExtraArgs = "-p zj-radar-plugin";
          doCheck = false; # wasm can't execute on the host builder; see `checks` for tests
        };
      cargoArtifactsWasm = craneLib.buildDepsOnly wasmArgs;
      zj-radar = craneLib.buildPackage (wasmArgs
        // {
          cargoArtifacts = cargoArtifactsWasm;
          doInstallCargoArtifacts = false;
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [pkgs.binaryen];
          # Install the wasm to $out/bin to match the Zellij-plugin convention
          # (e.g. zjstatus → ${pkgs.zjstatus}/bin/zjstatus.wasm), so downstream
          # layouts reference ${pkg}/bin/zj_radar.wasm like every other plugin.
          #
          # binaryen's `-Oz` post-pass on the way: Zellij runs the plugin under
          # the wasmi interpreter, where executed instructions are the cost,
          # and this pass measured 12–27 % less fuel on every event class
          # (`tools/wasm-fuel`, docs/design.md §15) plus ~13 % fewer bytes,
          # over rustc's own opt-level "z". The `--enable-*` set is exactly
          # what rustc emits for wasm32-wasip1 (`rustc --print cfg --target
          # wasm32-wasip1 | grep target_feature`, as of rustc 1.97); the
          # release wasm is stripped, so binaryen cannot detect them itself,
          # and `--all-features` would let it introduce encodings wasmi 0.51
          # rejects. Drift fails closed: a feature the list lacks makes
          # wasm-opt reject the module rather than mis-optimize. Dev builds
          # (`just dev`, E2E) stay unoptimized — the pass is behavior-
          # preserving and the release funnel exercises the shipped file.
          installPhaseCommand = ''
            mkdir -p $out/bin
            wasm-opt -Oz \
              --enable-bulk-memory --enable-multivalue --enable-mutable-globals \
              --enable-nontrapping-float-to-int --enable-reference-types --enable-sign-ext \
              target/wasm32-wasip1/release/zj_radar.wasm -o $out/bin/zj_radar.wasm
          '';
        });

      # ── host-target deps shared by the test/clippy checks ──
      cargoArtifactsHost = craneLib.buildDepsOnly commonArgs;

      # ── native CLI (host target; `crates/cli` workspace member) ──
      # The CLI embeds the wasm (build.rs → include_bytes!). Feed it the wasm
      # built above via ZJ_RADAR_WASM_PATH so the build doesn't recurse into a
      # nested wasm compile (which the crane sandbox can't do).
      cliArgs =
        commonArgs
        // {
          cargoExtraArgs = "-p zj-radar";
          ZJ_RADAR_WASM_PATH = "${zj-radar}/bin/zj_radar.wasm";
        };
      cargoArtifactsCli = craneLib.buildDepsOnly (commonArgs // {cargoExtraArgs = "-p zj-radar";});
      zj-radar-cli = craneLib.buildPackage (cliArgs
        // {
          pname = "zj-radar-cli";
          cargoArtifacts = cargoArtifactsCli;
          cargoExtraArgs = "--bin zj-radar";
          doCheck = false;
        });
    in {
      packages.default = zj-radar;
      packages.zj-radar = zj-radar;
      packages.zj-radar-cli = zj-radar-cli;

      checks = {
        inherit zj-radar;
        # The workspace checks compile the CLI member, whose build.rs embeds a
        # wasm: without ZJ_RADAR_WASM_PATH it falls back to a NESTED
        # `cargo build --target wasm32-wasip1` into the same workspace, which
        # deadlocks inside the sandbox (the inner cargo waits on the build-dir
        # lock the outer cargo holds while running build scripts, with no
        # network to fail fast) — the job hangs until the CI timeout. Feed the
        # prebuilt wasm here exactly like cliArgs does.
        #
        # Deliberately NOT --all-features (unlike ci.yml's cargo test): the
        # only extra feature is `e2e`, whose tests drive a live Zellij PTY —
        # impossible inside the nix sandbox.
        clippy = craneLib.cargoClippy (commonArgs
          // {
            cargoArtifacts = cargoArtifactsHost;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
            ZJ_RADAR_WASM_PATH = "${zj-radar}/bin/zj_radar.wasm";
          });
        test = craneLib.cargoTest (commonArgs
          // {
            cargoArtifacts = cargoArtifactsHost;
            ZJ_RADAR_WASM_PATH = "${zj-radar}/bin/zj_radar.wasm";
          });
        # NOT redundant with test/clippy above: `-p zj-radar` builds the CLI
        # standalone (only its own + core's features), reproducing what a
        # crates.io `cargo install zj-radar` sees — the workspace-wide checks
        # unify features across the plugin member and can mask a missing
        # feature declaration in the CLI's own manifest.
        cli-test = craneLib.cargoTest (cliArgs
          // {
            cargoArtifacts = cargoArtifactsCli;
          });
        cli-clippy = craneLib.cargoClippy (cliArgs
          // {
            cargoArtifacts = cargoArtifactsCli;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
      };

      devShells.default = pkgs.mkShell {
        # `just`, `bats`, and `shellcheck` are required by the CI recipes
        # (`just test-bash` / `just test-e2e`, and the `shellcheck` step) — keep
        # them here so `nix develop -c just …` resolves on PATH. `jq` is used by
        # the bash hook tests. `cargo-deny` reproduces the nightly advisory job
        # (`cargo deny check`).
        packages = [
          toolchain
          pkgs.jq
          pkgs.zellij
          pkgs.just
          pkgs.bats
          pkgs.shellcheck
          pkgs.cargo-deny
        ];
        shellHook = ''
          echo "zj-radar dev shell: $(rustc --version)"
          echo "dev:    just dev         (sandboxed zj-radar-dev session)"
          echo "build:  just dev-build   (wasm + CLI, no launch)"
          echo "test:   just test        (cargo test --all-features)"
          echo "        just test-bash   (shellcheck + bats hook tests)"
          echo "        just test-e2e    (wasm build + live Zellij PTY)"
        '';
      };
    });
}
