// Binary (WASI command) entry point for the zj-radar Zellij plugin.
//
// Zellij loads plugins as commands: it calls the wasm `_start` export, then
// `load`/`update`/`render`/`pipe`. `register_plugin!` generates exactly those
// (its `fn main` → `_start`), so the wasm artifact MUST be a binary crate, not
// a cdylib. All real logic lives in the `zj_radar` library; this file is only
// the macro wiring, gated to wasm so a host `cargo test`/`cargo build` links a
// trivial native binary instead of the unresolved wasm host imports.

#[cfg(target_arch = "wasm32")]
use zellij_tile::prelude::*;

#[cfg(target_arch = "wasm32")]
zellij_tile::register_plugin!(zj_radar::State);

#[cfg(not(target_arch = "wasm32"))]
fn main() {}
