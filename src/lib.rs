// The `cli` feature pulls clap/toml_edit/dirs, which don't build for wasm. The
// wasm plugin depends on this crate with `default-features = false`, so this
// only fires on a stray bare wasm build (e.g. `cargo build --target
// wasm32-wasip1` with no `-p`), turning a confusing clap-on-wasm error into a
// pointer at the right command.
#[cfg(all(target_arch = "wasm32", feature = "cli"))]
compile_error!("build the wasm plugin with `-p zj-radar-plugin` (the `cli` feature can't target wasm)");

// Re-export the shared core so the CLI keeps addressing these as
// `crate::status`, `crate::payload`, … with no per-reference churn. `command`
// and `kind` are used only in test code (coherence guards in cli/agents/mod.rs);
// they are genuinely unused on a non-test host build.
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use zj_radar_core::{command, kind, payload, status};

#[cfg(feature = "cli")]
pub mod cli;
