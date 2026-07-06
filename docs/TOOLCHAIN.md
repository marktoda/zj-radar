# Toolchain

Native `cargo` builds everything — both host tests and the WASM plugin. The
`wasm32-wasip1` target is requested by `rust-toolchain.toml`, so a `rustup`-managed
toolchain installs it automatically the first time you build. `cargo test` (host
target) covers the pure-logic modules and needs nothing extra.

Dev tracks `stable`; the workspace MSRV is **Rust 1.95** (declared as
`rust-version` in the root `Cargo.toml`, enforced by CI's `msrv` job, which
builds with exactly that toolchain).

```sh
cargo test                                          # host tests
cargo build --release --target wasm32-wasip1 -p zj-radar-plugin   # the WASM plugin
# → target/wasm32-wasip1/release/zj_radar.wasm
```

To dogfood that release build through the normal install path:

```sh
cargo install --path crates/cli
zj-radar setup zellij --wasm target/wasm32-wasip1/release/zj_radar.wasm
```

## If your `cargo` lacks the `wasm32-wasip1` target

If you use a non-`rustup` Rust that doesn't pick up the target from
`rust-toolchain.toml` (e.g. a bare Nix-profile toolchain), you'll see
`can't find crate for std … wasm32-wasip1 may not be installed`. Either add the
target to that toolchain, or use the repo's Nix dev shell, which pins a Rust with
the target:

```sh
nix develop -c cargo build --release --target wasm32-wasip1 -p zj-radar-plugin
```

## Dev loop

```sh
just dev          # build wasm + CLI, launch a FRESH sandboxed zj-radar-dev-<hhmmss> session
```

Uses the ambient Rust toolchain (`rust-toolchain.toml` auto-installs the
wasm target on first build). In the Nix shell, prefix with `nix develop -c`.

Zellij 0.44 does not safely hot-reload plugins that were created by a layout:
`start-or-reload-plugin` opens a second pane instead. The dev loop therefore
never reloads in place — every iteration is a fresh, uniquely named
`zj-radar-dev-<hhmmss>` session (exited leftovers are swept; live sessions are
never killed), launched from a plain terminal (`zj-radar run` refuses to nest inside
Zellij) and fully sandboxed under `target/dev/data`, so it runs alongside your
real sessions without touching them or an installed zj-radar's assets.
