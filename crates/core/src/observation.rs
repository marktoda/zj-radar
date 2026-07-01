//! Resolved per-pane observation vocabulary shared by status and command sources.

use crate::kind::Kind;
use crate::status::Status;
use crate::wire::wire_enum;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
    /// Sticky task label for agent panes (first line of the user's prompt).
    /// Carried across the whole turn by `StatusStore::apply`'s merge; always
    /// empty for command-origin panes. Serde-defaulted so pre-task snapshots
    /// still load.
    #[serde(default)]
    pub task: String,
    /// The source kind that produced this observation. Classified once at intake
    /// (`StatusStore::apply` / `command_kind`); the renderer reads it directly
    /// rather than re-parsing a string. Serializes under the `source` wire key as
    /// its `as_source()` token, so the persisted snapshot format is unchanged.
    #[serde(rename = "source")]
    pub kind: Kind,
    pub last_change_tick: u64,
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
    /// branch, and are active by definition, so those fields take fixed defaults;
    /// callers pass only what varies and override `exit_code` via struct-update
    /// when a command exits.
    pub fn command(status: Status, repo: String, msg: String, kind: Kind, tick: u64) -> Self {
        Self {
            origin: ObservationOrigin::Command,
            status,
            repo,
            branch: String::new(),
            msg,
            task: String::new(),
            kind,
            last_change_tick: tick,
            ever_active: true,
            exit_code: None,
        }
    }
}

/// A map of pane id → resolved observation, plus the lifecycle every source
/// shares (`prune`, snapshot insert). Focus no longer touches the store — the rail
/// shows what was pushed until a new broadcast, the exit-clear, or a prune. Both
/// `StatusStore` and `CommandStore` *contain* one of these and delegate to it — the
/// "two sources" split lives only in their intake (`apply` vs the command debounce
/// machine) and their "is it still live?" predicate, both of which they keep. There
/// is no trait here: there is no runtime heterogeneity to dispatch over, so a shared
/// struct by composition is the whole seam. The precedence *between* the two stores
/// stays in `RadarState`, never here.
#[derive(Default)]
pub struct ObservationStore {
    map: HashMap<u32, TrackedObservation>,
}

impl ObservationStore {
    pub fn get(&self, pane_id: u32) -> Option<&TrackedObservation> {
        self.map.get(&pane_id)
    }

    pub fn get_mut(&mut self, pane_id: u32) -> Option<&mut TrackedObservation> {
        self.map.get_mut(&pane_id)
    }

    pub fn insert(&mut self, pane_id: u32, observation: TrackedObservation) {
        self.map.insert(pane_id, observation);
    }

    pub fn prune(&mut self, live: &HashSet<u32>) {
        self.map.retain(|id, _| live.contains(id));
    }

    pub fn observations(&self) -> impl Iterator<Item = (u32, &TrackedObservation)> {
        self.map.iter().map(|(&pane_id, observation)| (pane_id, observation))
    }

    /// Does any observation satisfy `pred`? The two stores resting-state predicates
    /// differ (`StatusStore` counts any non-idle, `CommandStore` only `Running`),
    /// so each passes its own closure rather than sharing one definition.
    pub fn any(&self, pred: impl Fn(&TrackedObservation) -> bool) -> bool {
        self.map.values().any(pred)
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
            task: "fix e2e".into(),
            kind: Kind::Build,
            last_change_tick: 7,
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
        assert!(json.contains(r#""task":"fix e2e""#), "task persists in snapshots: {json}");
        assert_eq!(serde_json::from_str::<TrackedObservation>(&json).unwrap(), obs);
    }

    #[test]
    fn deserialize_is_lenient_on_status_but_strict_on_origin() {
        // Optional fields may be absent (serde defaults), and an unknown status
        // degrades to Idle — matching the pipe payload's `from_wire` contract.
        let json = r#"{"origin":"command","status":"???","repo":"","branch":"","msg":"","source":"","last_change_tick":0,"ever_active":false}"#;
        let obs: TrackedObservation = serde_json::from_str(json).unwrap();
        assert_eq!(obs.status, Status::Idle);
        assert_eq!(obs.exit_code, None);
        assert_eq!(obs.task, "", "pre-task snapshots load with an empty label");
        // An unknown origin is rejected so a corrupt entry can't masquerade as a
        // valid one — the snapshot loader drops the whole snapshot instead.
        let bad = json.replace(r#""origin":"command""#, r#""origin":"???""#);
        assert!(serde_json::from_str::<TrackedObservation>(&bad).is_err());
    }
}
