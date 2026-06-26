# Toolchain

The host's Nix-provisioned Rust has no `wasm32-wasip1` standard library and no
`rustup`, so the plugin cannot be built to WASM with the bare `cargo` on PATH.
`cargo test` (host target) works fine for the pure-logic modules.

## Building the WASM plugin

Use the dev shell, which pins a Rust toolchain with the `wasm32-wasip1` target:

```sh
nix develop          # enter the shell (first run fetches the toolchain)
cargo build --release --target wasm32-wasip1
# → target/wasm32-wasip1/release/zj_agents.wasm
```

Or run a one-off without entering the shell:

```sh
nix develop -c cargo build --release --target wasm32-wasip1
```

For the hot-reload dev layout (`dev/dev.kdl`) use the debug build:

```sh
nix develop -c cargo build --target wasm32-wasip1
zellij --layout dev/dev.kdl
```

`cargo test` does not need the dev shell — the pure modules are
`zellij-tile`-free and run on the host target.
