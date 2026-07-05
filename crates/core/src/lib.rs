//! zj-radar shared core: the versioned `zj_radar.status.v1` wire schema and the
//! pure status/command classification used by both the host CLI and the wasm
//! sidebar plugin. No clap, no zellij-tile — serde only.
//!
//! External producers emit the status contract by building a [`StatusPayload`]
//! (use `..Default::default()` for every field you don't set — it means exactly
//! "absent on the wire") and broadcasting [`to_wire`]'s JSON on the
//! `zj_radar.status.v1` Zellij pipe:
//!
//! ```
//! use zj_radar_core::{to_wire, Status, StatusPayload};
//!
//! let json = to_wire(&StatusPayload {
//!     pane_id: 12,
//!     status: Status::Running,
//!     source: "claude".into(),
//!     msg: "running tests".into(),
//!     ..Default::default()
//! });
//! // e.g. `zellij pipe --name zj_radar.status.v1 -- "$json"`
//! # assert!(json.contains(r#""status":"running""#));
//! ```

#![forbid(unsafe_code)]

pub mod command;
pub mod kind;
pub mod observation;
pub mod payload;
pub mod status;
pub mod wire;

// The friendly external surface, re-exported at the root. In-repo consumers
// keep using the module paths.
pub use kind::Kind;
pub use payload::{parse, to_wire, StatusPayload};
pub use status::Status;
