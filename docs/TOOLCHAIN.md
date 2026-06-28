# Toolchain

The host's Nix-provisioned Rust has no `wasm32-wasip1` standard library and no
`rustup`, so the plugin cannot be built to WASM with the bare `cargo` on PATH.
`cargo test` (host target) works fine for the pure-logic modules.

## Building the WASM plugin

Use the dev shell, which pins a Rust toolchain with the `wasm32-wasip1` target:

```sh
nix develop          # enter the shell (first run fetches the toolchain)
cargo build --release --target wasm32-wasip1
# → target/wasm32-wasip1/release/zj_radar.wasm
```

To dogfood that release build through the normal install path:

```sh
cargo install --path . --features cli
zj-radar setup zellij --wasm target/wasm32-wasip1/release/zj_radar.wasm
```

Or run a one-off without entering the shell:

```sh
nix develop -c cargo build --release --target wasm32-wasip1
```

For the dogfood dev layout (`dev/dev.kdl`) use the single dev entrypoint:

```sh
./dev/run.sh
```

The command works from either a normal terminal or inside Zellij. It uses the
ambient Rust toolchain when it has `wasm32-wasip1`, and falls back to the repo's
Nix flake when it does not.

Zellij 0.44 does not safely hot-reload plugins that were created by a layout:
`start-or-reload-plugin` opens a second pane instead. `./dev/run.sh` builds the
debug wasm, writes a generated layout with an absolute plugin path, and restarts
the disposable `zj-radar-dev` session when run from a normal terminal. Inside
Zellij, it finds the existing zj-radar sidebar panes in the current session and
replaces each one with `launch-plugin --in-place`, closing replaced panes and
skipping the plugin cache while preserving the rest of the session. Use
`./dev/run.sh --fresh-session` when you want the older clean-session behavior.

`cargo test` does not need the dev shell — the pure modules and the
host-testable session filesystem module are `zellij-tile`-free and run on the
host target.
