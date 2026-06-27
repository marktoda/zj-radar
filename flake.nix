{
  description = "zj-radar — Zellij sidebar plugin for AI-agent status (dev shell with wasm32-wasip1 toolchain)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, fenix, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        fx = fenix.packages.${system};
        # Host toolchain + the wasm32-wasip1 std the Zellij plugin compiles against.
        toolchain = fx.combine [
          fx.stable.toolchain
          fx.targets.wasm32-wasip1.stable.rust-std
        ];
      in {
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
