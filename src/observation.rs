//! Resolved per-pane observation vocabulary shared by status and command sources.

use crate::status::Status;
use crate::wire::wire_enum;
use serde::{Deserialize, Serialize};

wire_enum! {
    /// Which source produced an observation: the status pipe (agents) or a
    /// tracked shell command. Carries its own snapshot wire vocabulary, like
    /// `Status` — the persisted snapshot is the only place these tokens cross a
    /// boundary. Strict: an unknown origin deserializes to an *error*, so the
    /// snapshot loader drops a corrupt entry rather than guessing a source.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum ObservationOrigin {
        StatusPipe => "status_pipe",
        Command => "command",
    }
}

/// A resolved observation for one pane. `#[serde(...)]` makes this the persisted
/// snapshot record directly: the enum fields serialize as their wire tokens (see
/// the `Status`/`ObservationOrigin` impls) and the optional fields default when
/// absent, so older snapshots still load. There is no separate snapshot mirror
/// struct — this type *is* the v2 record (wrapped only with its `pane_id` key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedObservation {
    pub origin: ObservationOrigin,
    pub status: Status,
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub source: String,
    pub last_change_tick: u64,
    #[serde(default)]
    pub seq: Option<u64>,
    #[serde(default)]
    pub on_focus: Option<Status>,
    pub ever_active: bool,
    /// Exit code of a finished command pane, when known. Set by
    /// `CommandStore::on_exit` from a `zellij run`-style pane exit; `None` for
    /// agents (status pipe) and for commands that finish by returning to the
    /// shell prompt (no exit code is reported). Drives the `(exit N)` outcome
    /// tag on error rows.
    #[serde(default)]
    pub exit_code: Option<i32>,
}

impl TrackedObservation {
    /// A freshly-resolved command-origin observation. Command panes carry no VCS
    /// branch and no payload sequence, and are active by definition, so those
    /// fields take fixed defaults; callers pass only what varies and override
    /// `on_focus` / `exit_code` via struct-update when a command exits.
    pub fn command(status: Status, repo: String, msg: String, source: String, tick: u64) -> Self {
        Self {
            origin: ObservationOrigin::Command,
            status,
            repo,
            branch: String::new(),
            msg,
            source,
            last_change_tick: tick,
            seq: None,
            on_focus: None,
            ever_active: true,
            exit_code: None,
        }
    }

    /// Apply a pending `on_focus` transition (clear-on-focus): adopt the queued
    /// status and clear it. `last_change_tick` advances only when the status
    /// actually changes. Shared by `StatusStore` and `CommandStore`; the
    /// transition belongs to the observation, not the store.
    pub fn apply_on_focus(&mut self, tick: u64) {
        if let Some(next) = self.on_focus.take() {
            if self.status != next {
                self.last_change_tick = tick;
            }
            self.status = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_wire_round_trips_and_rejects_unknown() {
        for &origin in ObservationOrigin::ALL {
            assert_eq!(ObservationOrigin::from_wire(origin.as_wire()), Some(origin));
        }
        assert_eq!(ObservationOrigin::from_wire("nonsense"), None);
        assert_eq!(ObservationOrigin::from_wire(""), None);
    }

    fn sample() -> TrackedObservation {
        TrackedObservation {
            origin: ObservationOrigin::Command,
            status: Status::Error,
            repo: "zj-radar".into(),
            branch: "main".into(),
            msg: "cargo build".into(),
            source: "build".into(),
            last_change_tick: 7,
            seq: Some(3),
            on_focus: Some(Status::Idle),
            ever_active: true,
            exit_code: Some(1),
        }
    }

    #[test]
    fn serializes_enum_fields_as_wire_tokens_and_round_trips() {
        let obs = sample();
        let json = serde_json::to_string(&obs).unwrap();
        // Enum fields persist as their wire vocabulary, not serde's default
        // variant names — so the snapshot format is stable and human-legible.
        assert!(json.contains(r#""origin":"command""#), "origin token: {json}");
        assert!(json.contains(r#""status":"error""#), "status token: {json}");
        assert!(json.contains(r#""on_focus":"idle""#), "on_focus token: {json}");
        assert_eq!(serde_json::from_str::<TrackedObservation>(&json).unwrap(), obs);
    }

    #[test]
    fn deserialize_is_lenient_on_status_but_strict_on_origin() {
        // Optional fields may be absent (serde defaults), and an unknown status
        // degrades to Idle — matching the pipe payload's `from_wire` contract.
        let json = r#"{"origin":"command","status":"???","repo":"","branch":"","msg":"","source":"","last_change_tick":0,"ever_active":false}"#;
        let obs: TrackedObservation = serde_json::from_str(json).unwrap();
        assert_eq!(obs.status, Status::Idle);
        assert_eq!(obs.seq, None);
        assert_eq!(obs.exit_code, None);
        // An unknown origin is rejected so a corrupt entry can't masquerade as a
        // valid one — the snapshot loader drops the whole snapshot instead.
        let bad = json.replace(r#""origin":"command""#, r#""origin":"???""#);
        assert!(serde_json::from_str::<TrackedObservation>(&bad).is_err());
    }
}
