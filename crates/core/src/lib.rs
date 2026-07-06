//! zj-radar shared core: the versioned `zj_radar.status.v1` wire schema and the
//! pure status/command classification used by both the host CLI and the wasm
//! sidebar plugin. No clap, no zellij-tile — serde only.
//!
//! External producers emit the status contract by building a [`StatusPayload`]
//! (use `..Default::default()` for every field you don't set — it means exactly
//! "absent on the wire") and broadcasting [`to_wire`]'s JSON on the
//! [`STATUS_PIPE_NAME`] Zellij pipe. Don't hand-type the pipe name: the version
//! rides in it, and the const is the contract. Free-text fields are capped and
//! sanitized on the consumer side (see the `payload::MAX_*` consts), so long
//! messages degrade to truncation, never to a dropped update.
//!
//! ```
//! use zj_radar_core::{to_wire, Status, StatusPayload, STATUS_PIPE_NAME};
//!
//! let json = to_wire(&StatusPayload {
//!     pane_id: 12,
//!     status: Status::Running,
//!     source: "claude".into(),
//!     msg: "running tests".into(),
//!     ..Default::default()
//! });
//! // then: `zellij pipe --name $STATUS_PIPE_NAME -- "$json"`
//! # assert_eq!(STATUS_PIPE_NAME, "zj_radar.status.v1");
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
pub use payload::{parse, to_wire, StatusPayload, STATUS_PIPE_NAME};
pub use status::Status;
