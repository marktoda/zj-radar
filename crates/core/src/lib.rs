//! zj-radar shared core: the versioned `zj_radar.status.v1` wire schema and the
//! pure status/command classification used by both the host CLI and the wasm
//! sidebar plugin. No clap, no zellij-tile — serde only.
//!
//! The **stable surface** for external producers is the root re-exports below
//! ([`StatusPayload`], [`parse`], [`to_wire`], [`Status`], [`Kind`],
//! [`STATUS_PIPE_NAME`]) plus the [`payload`] module's `MAX_*` contract
//! consts. Everything else is workspace-internal plumbing shared with the
//! zj-radar plugin and CLI, with no stability guarantee.
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

// `command`, `observation`, and `wire` are internal to the zj-radar workspace
// (consumed by the plugin and CLI crates) with no stability guarantee — hidden
// from the published docs so the stable surface above is what producers see.
#[doc(hidden)]
pub mod command;
pub mod kind;
#[doc(hidden)]
pub mod observation;
pub mod payload;
pub mod pipe;
pub mod status;
#[doc(hidden)]
pub mod wire;

// The friendly external surface, re-exported at the root. In-repo consumers
// keep using the module paths.
pub use kind::Kind;
pub use payload::{parse, to_wire, StatusPayload, STATUS_PIPE_NAME};
pub use pipe::{self_limiting_pipe_argv, DEFAULT_PIPE_TIMEOUT_SECS};
pub use status::Status;
