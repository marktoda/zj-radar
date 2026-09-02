# Toolchain

Native `cargo` builds everything, host tests and the wasm plugin alike. The
`wasm32-wasip1` target is requested by `rust-toolchain.toml`, so a
`rustup`-managed toolchain installs it on the first build.

Dev tracks `stable`. The workspace MSRV is **Rust 1.95** (`rust-version` in the
root `Cargo.toml`, enforced by CI's `msrv` job).

```sh
cargo test                                                        # host tests
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin   # → target/wasm32-wasip1/release/zj_radar.wasm
```

To install that build through the normal path:

```sh
cargo install --path crates/cli
zj-radar setup zellij --wasm target/wasm32-wasip1/release/zj_radar.wasm
```

## If your `cargo` lacks the `wasm32-wasip1` target

A non-`rustup` Rust (a bare Nix-profile toolchain, say) ignores
`rust-toolchain.toml` and fails with `can't find crate for std …
wasm32-wasip1 may not be installed`. Add the target to that toolchain, or use
the repo's Nix shell, which pins a Rust that has it:

```sh
nix develop -c cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
```

## Nix flake outputs

```sh
nix develop                # dev shell: pinned Rust (+ wasm32-wasip1 std), just, bats, shellcheck, jq, zellij, cargo-deny
nix build .#zj-radar       # the wasm plugin → result/bin/zj_radar.wasm
nix build .#zj-radar-cli   # the native CLI (embeds that wasm) → result/bin/zj-radar
nix flake check            # hermetic clippy + tests + wasm build, what CI's `hermetic` job runs
```

The dev loop (`just dev`) is described in
[`CONTRIBUTING.md`](../CONTRIBUTING.md#dev-loop).
