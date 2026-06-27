//! Per-pane agent state, keyed by terminal pane id. No zellij-tile dependency.

use crate::payload::StatusPayload;
use crate::status::Status;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct AgentState {
    pub status: Status,
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub last_change_tick: u64,
    pub seq: Option<u64>,
    pub on_focus: Option<Status>,
    pub ever_active: bool,
}

#[derive(Default)]
pub struct StateStore {
    map: HashMap<u32, AgentState>,
}

impl StateStore {
    /// Apply an incoming payload. Drops out-of-order updates (seq <= stored seq).
    pub fn apply(&mut self, p: StatusPayload, tick: u64) {
        if let (Some(existing), Some(incoming)) = (self.map.get(&p.pane_id).and_then(|s| s.seq), p.seq) {
            if incoming <= existing {
                return;
            }
        }
        let prev_status = self.map.get(&p.pane_id).map(|s| s.status);
        let status_changed = prev_status != Some(p.status);
        let last_change_tick = if status_changed {
            tick
        } else {
            self.map.get(&p.pane_id).map(|s| s.last_change_tick).unwrap_or(tick)
        };
        let ever_active = p.status.is_active()
            || self.map.get(&p.pane_id).map(|s| s.ever_active).unwrap_or(false);
        self.map.insert(
            p.pane_id,
            AgentState {
                status: p.status,
                repo: p.repo,
                branch: p.branch,
                msg: p.msg,
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
            if let Some(next) = s.on_focus.take() {
                if s.status != next {
                    s.last_change_tick = tick;
                }
                s.status = next;
            }
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
}
