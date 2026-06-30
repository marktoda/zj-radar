//! zj-radar shared core: the versioned `zj_radar.status.v1` wire schema and the
//! pure status/command classification used by both the host CLI and the wasm
//! sidebar plugin. No clap, no zellij-tile — serde + unicode-width only.

pub mod command;
pub mod kind;
pub mod observation;
pub mod payload;
pub mod status;
pub mod wire;
