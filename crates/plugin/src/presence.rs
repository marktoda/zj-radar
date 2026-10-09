//! The presence record lives in core (`zj_radar_core::presence`) so the CLI's
//! `state` command parses exactly what the rail writes. Re-exported here so
//! plugin code keeps addressing it as `crate::presence`.
pub(crate) use zj_radar_core::presence::*;
