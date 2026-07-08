//! Resolved per-pane observation vocabulary shared by status and command sources.

use crate::kind::Kind;
use crate::payload::{sanitize, MAX_BRANCH_CHARS, MAX_MSG_CHARS, MAX_REPO_CHARS, MAX_TASK_CHARS};
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
    /// (`StatusStore::apply` / `command::classify`); the renderer reads it directly
    /// rather than re-parsing a string. Serializes under the `source` wire key as
    /// its `as_source()` token, so the persisted snapshot format is unchanged.
    #[serde(rename = "source")]
    pub kind: Kind,
    pub last_change_tick: u64,
    pub ever_active: bool,
    /// Exit code of a finished command pane, when known. Set by
    /// `CommandStore::on_exit` from a `zellij run`-style pane exit; `None` for
    /// agents (status pipe) and for commands that finish by returning to the
    /// shell prompt (no exit code is reported). Drives the `exit N` outcome
    /// tag on error rows.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Wall-clock second (Unix epoch) at which this observation first became
    /// `Done`/`Error`. `None` while live/idle. Rides the snapshot so a
    /// rehydrating instance ledgers the completion with the ORIGINAL stamp —
    /// without it, merged ledgers diverge (spec §4.3).
    #[serde(default)]
    pub completed_epoch_s: Option<u64>,
    /// Wall-clock second at which this observation entered `Pending` — when
    /// the agent started waiting on the user. `None` for every other status
    /// (`StatusStore::apply` owns the stamping, mirroring `completed_epoch_s`:
    /// stamped on the edge, kept across identical re-broadcasts, re-stamped
    /// when a *different* question arrives). Drives the rail's `· 12m`
    /// wait tag; rides the snapshot so rehydrated rows keep the true wait.
    #[serde(default)]
    pub pending_epoch_s: Option<u64>,
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
            completed_epoch_s: None,
            pending_epoch_s: None,
        }
    }

    /// Re-scrub the free-text fields with the same sanitizer and caps live
    /// intake applies at parse. Live observations are sanitized already; this
    /// is for observations loaded off disk, where a pre-sanitize build (or a
    /// hand-edited snapshot) may have persisted raw control characters that
    /// would otherwise flow straight into the rendered grid.
    pub fn sanitized(mut self) -> Self {
        self.repo = sanitize(&self.repo, MAX_REPO_CHARS);
        self.branch = sanitize(&self.branch, MAX_BRANCH_CHARS);
        self.msg = sanitize(&self.msg, MAX_MSG_CHARS);
        self.task = sanitize(&self.task, MAX_TASK_CHARS);
        self
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

    /// `pub(crate)` on purpose: the one accessor that can mutate a status
    /// without producing the displaced-completion recede `insert`/`prune`
    /// return, so it stays out of the published API to protect the
    /// ledger-edge discipline (its only user, `CommandStore`, wires its own
    /// recedes).
    pub(crate) fn get_mut(&mut self, pane_id: u32) -> Option<&mut TrackedObservation> {
        self.map.get_mut(&pane_id)
    }

    /// Insert, returning the observation this one displaced (if any) — the hook
    /// recede edges ride: a Done/Error coming back out of `insert`/`prune` is a
    /// completion leaving the card, which `RadarState` may ledger.
    pub fn insert(&mut self, pane_id: u32, observation: TrackedObservation) -> Option<TrackedObservation> {
        self.map.insert(pane_id, observation)
    }

    /// Prune every entry not in `live`, returning *every* dropped observation.
    /// The two consumers split the vec's roles without a pre-cut here: emptiness
    /// answers "did persisted state change" (an unpersisted Pending/Running drop
    /// leaves the shared snapshot carrying a status no live store holds, and
    /// late-spawned instances resurrect it — see `RadarState::panes_changed`),
    /// and the ledger sink filters to the completions worth ledgering itself
    /// (`LedgerEntry::from_observation` is `None` for anything else). Returning
    /// only the completions would re-open the trap where a caller reads
    /// emptiness as "nothing removed" and skips the persist.
    pub fn prune(&mut self, live: &HashSet<u32>) -> Vec<(u32, TrackedObservation)> {
        let dropped: Vec<(u32, TrackedObservation)> = self
            .map
            .iter()
            .filter(|(id, _)| !live.contains(id))
            .map(|(&id, obs)| (id, obs.clone()))
            .collect();
        self.map.retain(|id, _| live.contains(id));
        dropped
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
            completed_epoch_s: None,
            pending_epoch_s: None,
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
    fn insert_returns_displaced_and_prune_returns_every_drop() {
        let mut s = ObservationStore::default();
        assert!(s.insert(1, sample()).is_none());
        let displaced = s.insert(1, TrackedObservation { status: Status::Done, ..sample() });
        assert_eq!(displaced.unwrap().status, Status::Error, "old entry comes back out");
        s.insert(2, sample());
        s.insert(3, TrackedObservation { status: Status::Running, ..sample() });
        let mut dropped = s.prune(&[2].into_iter().collect());
        dropped.sort_by_key(|(id, _)| *id);
        assert_eq!(dropped.len(), 2, "every drop comes back out — emptiness is the persist signal");
        assert_eq!((dropped[0].0, dropped[0].1.status), (1, Status::Done));
        assert_eq!((dropped[1].0, dropped[1].1.status), (3, Status::Running), "non-completions included; the ledger sink filters");
        assert!(s.get(3).is_none());
        assert!(s.get(2).is_some());
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
