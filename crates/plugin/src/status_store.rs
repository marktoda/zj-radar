//! Per-pane status-payload observations, keyed by terminal pane id.
//! No zellij-tile dependency.

use crate::kind::Kind;
use crate::observation::{ObservationOrigin, ObservationStore, TrackedObservation};
use crate::payload::StatusPayload;
use std::collections::HashSet;

#[derive(Default)]
pub struct StatusStore {
    store: ObservationStore,
}

impl StatusStore {
    /// Apply an incoming payload. Latest broadcast wins (the pipe delivers in
    /// order; no producer stamps a sequence, so there is nothing to reorder).
    pub fn apply(&mut self, p: StatusPayload, tick: u64) {
        let prev = self.store.get(p.pane_id);
        let status_changed = prev.map(|s| s.status) != Some(p.status);
        let last_change_tick = if status_changed {
            tick
        } else {
            prev.map(|s| s.last_change_tick).unwrap_or(tick)
        };
        let ever_active = p.status.is_active() || prev.is_some_and(|s| s.ever_active);
        self.store.insert(
            p.pane_id,
            TrackedObservation {
                origin: ObservationOrigin::StatusPipe,
                status: p.status,
                repo: p.repo,
                branch: p.branch,
                msg: p.msg,
                // Classify the untrusted wire `source` into a Kind once, here at
                // intake; the renderer reads `kind` directly (no re-parse).
                kind: Kind::from_source(&p.source),
                last_change_tick,
                on_focus: p.on_focus,
                ever_active,
                exit_code: None,
            },
        );
    }

    /// One-shot: when the exact pane is focused, apply its pending on_focus status.
    pub fn on_pane_focused(&mut self, pane_id: u32, tick: u64) {
        self.store.on_pane_focused(pane_id, tick);
    }

    /// Recede this pane's completion the instant it finishes under focus (Done
    /// only — see `TrackedObservation::recede_on_focus`). Focus-agnostic: the
    /// caller passes the focused pane id; the store just forwards. Distinct from
    /// `on_pane_focused`, which clears any state on a *visit* — this one is "you
    /// watched it finish".
    pub fn recede_if_focused(&mut self, pane_id: u32, tick: u64) {
        self.store.recede_if_focused(pane_id, tick);
    }

    pub fn prune(&mut self, live: &HashSet<u32>) {
        self.store.prune(live);
    }

    pub fn get(&self, pane_id: u32) -> Option<&TrackedObservation> {
        self.store.get(pane_id)
    }

    /// True if any observation is non-idle (`is_active`: Running/Pending/Done/Error).
    /// Deliberately broader than `CommandStore::has_pending_or_active` (which keys
    /// on `Running` only): a *finished* agent row stays "active" here so the timer
    /// keeps re-arming to refresh its elapsed-time display. Keep the two predicates
    /// distinct — they answer "should the timer tick?" for sources with different
    /// resting behaviour.
    pub fn any_active(&self) -> bool {
        self.store.any(|s| s.status.is_active())
    }

    pub(crate) fn observations(&self) -> impl Iterator<Item = (u32, &TrackedObservation)> {
        self.store.observations()
    }

    /// Insert a snapshot-loaded observation. The caller (`RadarState::load_snapshot`)
    /// owns origin routing — it `match`es on `observation.origin` to pick the store
    /// — so this trusts what it's handed rather than re-checking the origin.
    pub(crate) fn insert_snapshot_observation(
        &mut self,
        pane_id: u32,
        observation: TrackedObservation,
    ) {
        self.store.insert(pane_id, observation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::status::Status;

    fn payload(pane_id: u32, status: Status) -> StatusPayload {
        StatusPayload {
            pane_id,
            status,
            repo: "r".into(),
            branch: "b".into(),
            msg: "m".into(),
            on_focus: None,
            source: "test".into(),
        }
    }

    #[test]
    fn apply_sets_last_change_tick_only_on_status_change() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 5);
        assert_eq!(s.get(1).unwrap().last_change_tick, 5);
        s.apply(payload(1, Status::Running), 9); // same status
        assert_eq!(s.get(1).unwrap().last_change_tick, 5);
        s.apply(payload(1, Status::Done), 12); // changed
        assert_eq!(s.get(1).unwrap().last_change_tick, 12);
        // verify repo, branch, msg fields are set
        assert_eq!(s.get(1).unwrap().repo, "r");
        assert_eq!(s.get(1).unwrap().branch, "b");
        assert_eq!(s.get(1).unwrap().msg, "m");
    }

    #[test]
    fn on_focus_applies_once_then_clears() {
        let mut s = StatusStore::default();
        let mut p = payload(1, Status::Done);
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
    fn recede_if_focused_clears_done_but_not_error() {
        let mut s = StatusStore::default();
        let mut done = payload(1, Status::Done);
        done.on_focus = Some(Status::Idle);
        s.apply(done, 1);
        let mut err = payload(2, Status::Error);
        err.on_focus = Some(Status::Idle);
        s.apply(err, 1);

        s.recede_if_focused(1, 5);
        s.recede_if_focused(2, 5);

        assert_eq!(s.get(1).unwrap().status, Status::Idle, "Done recedes");
        assert_eq!(s.get(2).unwrap().status, Status::Error, "Error persists");
        // An unknown pane id is a no-op (never panics).
        s.recede_if_focused(999, 5);
    }

    #[test]
    fn prune_removes_dead_panes() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1);
        s.apply(payload(2, Status::Done), 1);
        let live: HashSet<u32> = [2].into_iter().collect();
        s.prune(&live);
        assert!(s.get(1).is_none());
        assert!(s.get(2).is_some());
    }

    #[test]
    fn ever_active_sticks_after_returning_to_idle() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1);
        s.apply(payload(1, Status::Idle), 2);
        assert!(s.get(1).unwrap().ever_active);
        assert!(!s.any_active());
    }

    #[test]
    fn idle_clears_a_prior_dones_message() {
        // Regression for the `/clear` stale-status bug: a finished pane carries a
        // `done` + message; the SessionStart{clear} hook broadcasts an empty
        // `idle`. The pane must drop its stale message and stop counting as
        // active, so the rail no longer shows the pre-clear line.
        let mut s = StatusStore::default();
        let mut done = payload(1, Status::Done);
        done.msg = "shipped the feature".into();
        s.apply(done, 1);
        assert_eq!(s.get(1).unwrap().msg, "shipped the feature");
        assert!(s.any_active());

        let mut idle = payload(1, Status::Idle);
        idle.msg = String::new();
        s.apply(idle, 2);

        assert_eq!(s.get(1).unwrap().status, Status::Idle);
        assert_eq!(s.get(1).unwrap().msg, "", "stale message is cleared");
        assert!(!s.any_active(), "a cleared pane no longer drives tab status");
    }
}
