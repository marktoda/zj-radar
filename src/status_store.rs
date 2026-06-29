//! Per-pane status-payload observations, keyed by terminal pane id.
//! No zellij-tile dependency.

use crate::observation::{ObservationOrigin, TrackedObservation};
use crate::payload::StatusPayload;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct StatusStore {
    map: HashMap<u32, TrackedObservation>,
}

impl StatusStore {
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
            TrackedObservation {
                origin: ObservationOrigin::StatusPipe,
                status: p.status,
                repo: p.repo,
                branch: p.branch,
                msg: p.msg,
                source: p.source,
                last_change_tick,
                seq: p.seq,
                on_focus: p.on_focus,
                ever_active,
                exit_code: None,
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

    pub fn get(&self, pane_id: u32) -> Option<&TrackedObservation> {
        self.map.get(&pane_id)
    }

    /// True if any observation is non-idle (`is_active`: Running/Pending/Done/Error).
    /// Deliberately broader than `CommandStore::has_pending_or_active` (which keys
    /// on `Running` only): a *finished* agent row stays "active" here so the timer
    /// keeps re-arming to refresh its elapsed-time display. Keep the two predicates
    /// distinct — they answer "should the timer tick?" for sources with different
    /// resting behaviour.
    pub fn any_active(&self) -> bool {
        self.map.values().any(|s| s.status.is_active())
    }

    pub(crate) fn observations(&self) -> impl Iterator<Item = (u32, &TrackedObservation)> {
        self.map
            .iter()
            .map(|(&pane_id, observation)| (pane_id, observation))
    }

    /// Insert a snapshot-loaded observation. The caller (`RadarState::load_snapshot`)
    /// owns origin routing — it `match`es on `observation.origin` to pick the store
    /// — so this trusts what it's handed rather than re-checking the origin.
    pub(crate) fn insert_snapshot_observation(
        &mut self,
        pane_id: u32,
        observation: TrackedObservation,
    ) {
        self.map.insert(pane_id, observation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    use crate::status::Status;

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
        let mut s = StatusStore::default();
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
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running, Some(10)), 1);
        s.apply(payload(1, Status::Done, Some(5)), 2); // stale
        assert_eq!(s.get(1).unwrap().status, Status::Running);
        s.apply(payload(1, Status::Done, Some(11)), 3); // newer
        assert_eq!(s.get(1).unwrap().status, Status::Done);
    }

    #[test]
    fn on_focus_applies_once_then_clears() {
        let mut s = StatusStore::default();
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
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running, None), 1);
        s.apply(payload(2, Status::Done, None), 1);
        let live: HashSet<u32> = [2].into_iter().collect();
        s.prune(&live);
        assert!(s.get(1).is_none());
        assert!(s.get(2).is_some());
    }

    #[test]
    fn ever_active_sticks_after_returning_to_idle() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running, None), 1);
        s.apply(payload(1, Status::Idle, None), 2);
        assert!(s.get(1).unwrap().ever_active);
        assert!(!s.any_active());
    }

    // ── proptest properties (ported from harness branch) ──

    proptest! {
        #[test]
        fn apply_order_independent_with_seq(seqs in proptest::collection::vec(0u64..20, 1..12)) {
            // Same payloads applied in arbitrary order vs sorted-by-seq must converge
            // to the same final status. The seq dedup filter (drop if incoming <= stored)
            // guarantees that the highest-seq payload always wins regardless of order.
            let mk = |seq: u64| StatusPayload {
                pane_id: 1, status: if seq % 2 == 0 { Status::Running } else { Status::Done },
                repo: "r".into(), branch: "".into(), msg: "".into(),
                on_focus: None, seq: Some(seq), source: "t".into(),
            };
            let mut a = StatusStore::default();
            for &s in &seqs { a.apply(mk(s), s); }

            let mut sorted = seqs.clone(); sorted.sort_unstable();
            let mut b = StatusStore::default();
            for &s in &sorted { b.apply(mk(s), s); }

            prop_assert_eq!(a.get(1).map(|x| x.status), b.get(1).map(|x| x.status));

            // Pin the dedup contract: the surviving status must be the one carried
            // by the MAX-seq payload (highest seq wins, not just "both agree").
            let max_seq = seqs.iter().max().unwrap();
            let expected_status = if max_seq % 2 == 0 { Status::Running } else { Status::Done };
            prop_assert_eq!(a.get(1).map(|x| x.status), Some(expected_status));
            prop_assert_eq!(b.get(1).map(|x| x.status), Some(expected_status));
        }
    }
}
