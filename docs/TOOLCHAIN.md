# Toolchain

Native `cargo` builds everything — both host tests and the WASM plugin. The
`wasm32-wasip1` target is requested by `rust-toolchain.toml`, so a `rustup`-managed
toolchain installs it automatically the first time you build. `cargo test` (host
target) covers the pure-logic modules and needs nothing extra.

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

For the dogfood dev layout (`dev/dev.kdl`) use the single dev entrypoint:

```sh
./dev/run.sh
```

The command works from either a normal terminal or inside Zellij. It uses the
ambient Rust toolchain when it has `wasm32-wasip1`, and falls back to the repo's
Nix flake when it does not.

Zellij 0.44 does not safely hot-reload plugins that were created by a layout:
`start-or-reload-plugin` opens a second pane instead. `./dev/run.sh` builds the
debug wasm and writes a generated layout with an absolute plugin path, then:

- **From a normal terminal** it (re)starts the disposable `zj-radar-dev` session
  against that layout — the full clean-slate dev surface.
- **Inside an existing Zellij session** it leaves the current session untouched
  (it will not hot-swap panes in a session you're working in) and prints a hint
  to use `--fresh-session`. Pass `./dev/run.sh --fresh-session` to switch your
  client to a fresh disposable dev session (it refuses to replace the session
  you're currently in). Re-run to pick up a new build.
