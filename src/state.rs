//! Per-pane agent state, keyed by terminal pane id. No zellij-tile dependency.

use crate::payload::StatusPayload;
use crate::status::Status;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// One pane's persisted record. `status`/`on_focus` use the same wire vocabulary
/// as the pipe payload (`Status::as_wire`) so the snapshot format never drifts
/// from the broadcast format.
#[derive(Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub pane_id: u32,
    pub status: String,
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub source: String,
    pub last_change_tick: u64,
    #[serde(default)]
    pub seq: Option<u64>,
    #[serde(default)]
    pub on_focus: Option<String>,
    pub ever_active: bool,
}

/// The whole `StateStore` plus the owning instance's `tick`, as written to the
/// shared `/cache` snapshot. `tick` is carried so a freshly-seeded instance
/// keeps `last_change_tick` values meaningful (elapsed-time display) instead of
/// resetting its clock to 0 underneath them.
#[derive(Serialize, Deserialize, Default)]
pub struct Snapshot {
    pub v: u32,
    pub tick: u64,
    pub panes: Vec<PaneSnapshot>,
}

/// Current snapshot schema version. Bumped only on an incompatible change; an
/// unrecognized version is ignored on load (the instance just starts empty and
/// the next broadcast repopulates it).
pub const SNAPSHOT_V: u32 = 1;

#[derive(Clone, Debug)]
pub struct AgentState {
    pub status: Status,
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub source: String,
    pub last_change_tick: u64,
    pub seq: Option<u64>,
    pub on_focus: Option<Status>,
    pub ever_active: bool,
}

impl AgentState {
    /// Apply a pending `on_focus` transition (clear-on-focus): adopt the queued
    /// status and clear it. `last_change_tick` advances only when the status
    /// actually changes. Shared by `StateStore` and `command::CommandStore`,
    /// which both hold `AgentState` — the transition belongs to the data, not
    /// the store.
    pub fn apply_on_focus(&mut self, tick: u64) {
        if let Some(next) = self.on_focus.take() {
            if self.status != next {
                self.last_change_tick = tick;
            }
            self.status = next;
        }
    }
}

#[derive(Default)]
pub struct StateStore {
    map: HashMap<u32, AgentState>,
}

impl StateStore {
    /// Apply an incoming payload. Drops out-of-order updates (seq <= stored seq).
    pub fn apply(&mut self, p: StatusPayload, tick: u64) {
        if let (Some(existing), Some(incoming)) =
            (self.map.get(&p.pane_id).and_then(|s| s.seq), p.seq)
        {
            if incoming <= existing {
                return;
            }
        }
        let prev_status = self.map.get(&p.pane_id).map(|s| s.status);
        let status_changed = prev_status != Some(p.status);
        let last_change_tick = if status_changed {
            tick
        } else {
            self.map
                .get(&p.pane_id)
                .map(|s| s.last_change_tick)
                .unwrap_or(tick)
        };
        let ever_active = p.status.is_active()
            || self
                .map
                .get(&p.pane_id)
                .map(|s| s.ever_active)
                .unwrap_or(false);
        self.map.insert(
            p.pane_id,
            AgentState {
                status: p.status,
                repo: p.repo,
                branch: p.branch,
                msg: p.msg,
                source: p.source,
                last_change_tick,
                seq: p.seq,
                on_focus: p.on_focus,
                ever_active,
            },
        );
    }

    /// One-shot: when the exact pane is focused, apply its pending on_focus status.
    pub fn on_pane_focused(&mut self, pane_id: u32, tick: u64) {
        if let Some(s) = self.map.get_mut(&pane_id) {
            s.apply_on_focus(tick);
        }
    }

    pub fn prune(&mut self, live: &HashSet<u32>) {
        self.map.retain(|id, _| live.contains(id));
    }

    pub fn get(&self, pane_id: u32) -> Option<&AgentState> {
        self.map.get(&pane_id)
    }

    pub fn any_active(&self) -> bool {
        self.map.values().any(|s| s.status.is_active())
    }

    /// Serialize the whole store (+ the owning instance's `tick`) to the shared
    /// snapshot JSON. Written to `/cache` so a newly-spawned per-tab instance can
    /// seed itself with state it never received over the broadcast pipe. Pane
    /// iteration order is unspecified (a `HashMap`); the consumer keys by id.
    pub fn to_json(&self, tick: u64) -> String {
        let panes = self
            .map
            .iter()
            .map(|(&pane_id, s)| PaneSnapshot {
                pane_id,
                status: s.status.as_wire().to_string(),
                repo: s.repo.clone(),
                branch: s.branch.clone(),
                msg: s.msg.clone(),
                source: s.source.clone(),
                last_change_tick: s.last_change_tick,
                seq: s.seq,
                on_focus: s.on_focus.map(|f| f.as_wire().to_string()),
                ever_active: s.ever_active,
            })
            .collect();
        let snap = Snapshot {
            v: SNAPSHOT_V,
            tick,
            panes,
        };
        serde_json::to_string(&snap).unwrap_or_default()
    }

    /// Rebuild a store from a snapshot JSON, returning it alongside the persisted
    /// `tick`. Returns `None` on invalid JSON or an unrecognized schema version —
    /// callers treat that as "start empty" (the next broadcast repopulates).
    /// `Status::from_wire` maps any unknown status to `Idle`, so a partially
    /// understood snapshot never errors.
    pub fn from_json(raw: &str) -> Option<(StateStore, u64)> {
        let snap: Snapshot = serde_json::from_str(raw).ok()?;
        if snap.v != SNAPSHOT_V {
            return None;
        }
        let mut map = HashMap::with_capacity(snap.panes.len());
        for p in snap.panes {
            map.insert(
                p.pane_id,
                AgentState {
                    status: Status::from_wire(&p.status),
                    repo: p.repo,
                    branch: p.branch,
                    msg: p.msg,
                    source: p.source,
                    last_change_tick: p.last_change_tick,
                    seq: p.seq,
                    on_focus: p.on_focus.as_deref().map(Status::from_wire),
                    ever_active: p.ever_active,
                },
            );
        }
        Some((StateStore { map }, snap.tick))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(pane_id: u32, status: Status, seq: Option<u64>) -> StatusPayload {
        StatusPayload {
            pane_id,
            status,
            repo: "r".into(),
            branch: "b".into(),
            msg: "m".into(),
            on_focus: None,
            seq,
            source: "test".into(),
        }
    }

    #[test]
    fn apply_sets_last_change_tick_only_on_status_change() {
        let mut s = StateStore::default();
        s.apply(payload(1, Status::Running, None), 5);
        assert_eq!(s.get(1).unwrap().last_change_tick, 5);
        s.apply(payload(1, Status::Running, None), 9); // same status
        assert_eq!(s.get(1).unwrap().last_change_tick, 5);
        s.apply(payload(1, Status::Done, None), 12); // changed
        assert_eq!(s.get(1).unwrap().last_change_tick, 12);
        // verify repo, branch, msg fields are set
        assert_eq!(s.get(1).unwrap().repo, "r");
        assert_eq!(s.get(1).unwrap().branch, "b");
        assert_eq!(s.get(1).unwrap().msg, "m");
    }

    #[test]
    fn out_of_order_seq_is_dropped() {
        let mut s = StateStore::default();
        s.apply(payload(1, Status::Running, Some(10)), 1);
        s.apply(payload(1, Status::Done, Some(5)), 2); // stale
        assert_eq!(s.get(1).unwrap().status, Status::Running);
        s.apply(payload(1, Status::Done, Some(11)), 3); // newer
        assert_eq!(s.get(1).unwrap().status, Status::Done);
    }

    #[test]
    fn on_focus_applies_once_then_clears() {
        let mut s = StateStore::default();
        let mut p = payload(1, Status::Done, None);
        p.on_focus = Some(Status::Idle);
        s.apply(p, 1);
        s.on_pane_focused(1, 7);
        assert_eq!(s.get(1).unwrap().status, Status::Idle);
        assert_eq!(s.get(1).unwrap().on_focus, None);
        // focusing again does nothing
        s.on_pane_focused(1, 9);
        assert_eq!(s.get(1).unwrap().status, Status::Idle);
    }

    #[test]
    fn prune_removes_dead_panes() {
        let mut s = StateStore::default();
        s.apply(payload(1, Status::Running, None), 1);
        s.apply(payload(2, Status::Done, None), 1);
        let live: HashSet<u32> = [2].into_iter().collect();
        s.prune(&live);
        assert!(s.get(1).is_none());
        assert!(s.get(2).is_some());
    }

    #[test]
    fn ever_active_sticks_after_returning_to_idle() {
        let mut s = StateStore::default();
        s.apply(payload(1, Status::Running, None), 1);
        s.apply(payload(1, Status::Idle, None), 2);
        assert!(s.get(1).unwrap().ever_active);
        assert!(!s.any_active());
    }

    // ── snapshot round-trip (the /cache rehydration seam) ──

    #[test]
    fn snapshot_round_trip_preserves_all_panes_and_tick() {
        let mut s = StateStore::default();
        // A running agent, and a done-with-pending-on_focus pane.
        s.apply(payload(1, Status::Running, Some(7)), 3);
        let mut p2 = payload(2, Status::Done, Some(9));
        p2.on_focus = Some(Status::Idle);
        p2.repo = "pinky".into();
        p2.branch = "fix/x".into();
        p2.msg = "shipped it".into();
        p2.source = "codex".into();
        s.apply(p2, 5);
        // Pane 2 went Running->Done at some point, so ever_active is set; make
        // pane 1 return to idle to exercise the ever_active-sticks path too.
        s.apply(payload(1, Status::Idle, Some(11)), 8);

        let json = s.to_json(42);
        let (restored, tick) = StateStore::from_json(&json).expect("valid snapshot");

        assert_eq!(tick, 42, "owning instance tick is carried");
        // Pane 1: idle now, but ever_active sticky, seq advanced.
        let a = restored.get(1).expect("pane 1 present");
        assert_eq!(a.status, Status::Idle);
        assert!(a.ever_active, "ever_active survives the round trip");
        assert_eq!(a.seq, Some(11));
        assert_eq!(a.last_change_tick, 8);
        // Pane 2: all string fields + on_focus survive verbatim.
        let b = restored.get(2).expect("pane 2 present");
        assert_eq!(b.status, Status::Done);
        assert_eq!(b.repo, "pinky");
        assert_eq!(b.branch, "fix/x");
        assert_eq!(b.msg, "shipped it");
        assert_eq!(b.source, "codex");
        assert_eq!(
            b.on_focus,
            Some(Status::Idle),
            "clear-on-focus intent survives"
        );
        assert_eq!(b.seq, Some(9));
    }

    #[test]
    fn snapshot_seeded_done_pane_still_clears_on_focus() {
        // A new instance seeded from a snapshot must honor the queued clear-on-
        // focus transition, exactly as if the payload had arrived live.
        let mut s = StateStore::default();
        let mut p = payload(5, Status::Done, None);
        p.on_focus = Some(Status::Idle);
        s.apply(p, 1);
        let (mut restored, _) = StateStore::from_json(&s.to_json(2)).unwrap();
        restored.on_pane_focused(5, 9);
        assert_eq!(restored.get(5).unwrap().status, Status::Idle);
        assert_eq!(restored.get(5).unwrap().on_focus, None);
    }

    #[test]
    fn from_json_rejects_garbage_and_wrong_version() {
        assert!(StateStore::from_json("not json").is_none());
        // A structurally valid snapshot with an unknown version is ignored.
        let wrong = r#"{"v":999,"tick":1,"panes":[]}"#;
        assert!(StateStore::from_json(wrong).is_none());
        // The right version with no panes is a valid (empty) snapshot.
        let empty = format!(r#"{{"v":{SNAPSHOT_V},"tick":4,"panes":[]}}"#);
        let (store, tick) = StateStore::from_json(&empty).expect("empty is valid");
        assert_eq!(tick, 4);
        assert!(store.get(1).is_none());
    }

    #[test]
    fn empty_store_snapshot_round_trips() {
        let s = StateStore::default();
        let (restored, tick) = StateStore::from_json(&s.to_json(0)).unwrap();
        assert_eq!(tick, 0);
        assert!(!restored.any_active());
    }
}
