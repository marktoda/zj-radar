//! Per-pane status-payload observations, keyed by terminal pane id.
//! No zellij-tile dependency.

use crate::kind::Kind;
use crate::observation::{ObservationOrigin, ObservationStore, TrackedObservation};
use crate::payload::StatusPayload;
use crate::status::Status;
use std::collections::HashSet;

#[derive(Default)]
pub struct StatusStore {
    store: ObservationStore,
}

impl StatusStore {
    /// Apply an incoming payload. Latest broadcast wins (the pipe delivers in
    /// order; no producer stamps a sequence, so there is nothing to reorder).
    ///
    /// Returns the displaced observation IFF it was a completion (`Done`/`Error`)
    /// that a *real* edge just overwrote — status or message actually changed.
    /// An identical `(status, msg)` re-broadcast is a no-op edge: it returns
    /// `None` and does not re-stamp `completed_epoch_s`, so a completion's
    /// original wall-clock stamp survives repeated re-broadcasts of the same
    /// turn (spec §4.2/§4.3).
    pub fn apply(&mut self, p: StatusPayload, tick: u64, now_epoch_s: u64) -> Option<TrackedObservation> {
        let prev = self.store.get(p.pane_id);
        let status_changed = prev.map(|s| s.status) != Some(p.status);
        let identical = !status_changed && prev.is_some_and(|s| s.msg == p.msg);
        let last_change_tick = if status_changed {
            tick
        } else {
            prev.map(|s| s.last_change_tick).unwrap_or(tick)
        };
        let ever_active = p.status.is_active() || prev.is_some_and(|s| s.ever_active);
        // Sticky task label: a new prompt replaces it, taskless events (the
        // overwhelming majority — every tool hook) carry it forward, and idle
        // (`/clear`) resets it along with the msg.
        let task = if p.status == crate::status::Status::Idle {
            String::new()
        } else if p.task.is_empty() {
            prev.map(|s| s.task.clone()).unwrap_or_default()
        } else {
            p.task
        };
        let completed_epoch_s = match p.status {
            Status::Done | Status::Error if identical => prev.and_then(|s| s.completed_epoch_s),
            Status::Done | Status::Error => Some(now_epoch_s),
            _ => None,
        };
        let was_completion = prev.is_some_and(|s| matches!(s.status, Status::Done | Status::Error));
        let displaced = self.store.insert(
            p.pane_id,
            TrackedObservation {
                origin: ObservationOrigin::StatusPipe,
                status: p.status,
                repo: p.repo,
                branch: p.branch,
                msg: p.msg,
                task,
                // Classify the untrusted wire `source` into a Kind once, here at
                // intake; the renderer reads `kind` directly (no re-parse).
                kind: Kind::from_source(&p.source),
                last_change_tick,
                ever_active,
                exit_code: None,
                completed_epoch_s,
            },
        );
        if identical || !was_completion {
            None
        } else {
            displaced
        }
    }

    /// Clear a pane's pushed status to idle because its producer is gone — the
    /// pane returned to a shell prompt (`command::is_shell_prompt`). No-op if the
    /// pane isn't tracked, is already Idle, or is currently Running: a live agent
    /// turn re-asserts Running via its hooks, so a transient foreground flicker to
    /// a shell must never be mistaken for the agent exiting. Keeps repo/branch so
    /// the tab keeps its name; drops the message. Returns the observation this
    /// cleared (carrying its completion + stamp, if any) — the future ledger's
    /// recede edge for "the agent exited after finishing." Unlike the (removed)
    /// focus-driven recede, this rides the shared `CommandChanged` signal, so
    /// every tab's instance clears in lockstep.
    pub fn clear_on_prompt_return(&mut self, pane_id: u32, tick: u64) -> Option<TrackedObservation> {
        let prev = self.store.get(pane_id)?;
        if matches!(prev.status, Status::Running | Status::Idle) {
            return None;
        }
        let old = prev.clone();
        let repo = old.repo.clone();
        let branch = old.branch.clone();
        let kind = old.kind;
        let _ = self.store.insert(
            pane_id,
            TrackedObservation {
                origin: ObservationOrigin::StatusPipe,
                status: Status::Idle,
                repo,
                branch,
                msg: String::new(),
                task: String::new(),
                kind,
                last_change_tick: tick,
                ever_active: true,
                exit_code: None,
                completed_epoch_s: None,
            },
        );
        Some(old)
    }

    /// Prune panes no longer live, returning the dropped completions
    /// (`Done`/`Error`) — a pane closing with an unreceded completion still on
    /// it is a recede edge for the future ledger. Non-completion drops (Running,
    /// Idle, Pending) are filtered out here since they carry nothing to ledger.
    pub fn prune(&mut self, live: &HashSet<u32>) -> Vec<(u32, TrackedObservation)> {
        self.store
            .prune(live)
            .into_iter()
            .filter(|(_, obs)| matches!(obs.status, Status::Done | Status::Error))
            .collect()
    }

    pub fn get(&self, pane_id: u32) -> Option<&TrackedObservation> {
        self.store.get(pane_id)
    }

    /// True if any observation is currently `Running` — the one *animated* state
    /// (its glyph spins each tick). Matches `CommandStore::has_pending_or_active`'s
    /// narrowness on purpose: a finished `Done`/`Error` or a waiting `Pending` is
    /// terminal for tick purposes — it doesn't animate, and its notification/recede
    /// is a one-shot the settle carries — so it must NOT pin the timer awake. (An
    /// earlier version counted every non-idle status here to "refresh elapsed
    /// time," but elapsed time isn't rendered, so a backgrounded `Done` just spun
    /// the timer forever — see the timer-arming discussion in `runtime`.)
    pub fn any_running(&self) -> bool {
        self.store.any(|s| s.status == crate::status::Status::Running)
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
        let _ = self.store.insert(pane_id, observation);
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
            task: String::new(),
            source: "test".into(),
        }
    }

    fn payload_with_task(pane_id: u32, status: Status, task: &str) -> StatusPayload {
        StatusPayload { task: task.into(), ..payload(pane_id, status) }
    }

    #[test]
    fn task_is_sticky_across_taskless_updates_and_replaced_by_a_new_one() {
        let mut s = StatusStore::default();
        // UserPromptSubmit carries the task…
        s.apply(payload_with_task(1, Status::Running, "fix flaky e2e"), 1);
        assert_eq!(s.get(1).unwrap().task, "fix flaky e2e");
        // …PreToolUse / Stop broadcasts don't (empty task) — the label sticks.
        s.apply(payload(1, Status::Running), 2);
        s.apply(payload(1, Status::Done), 3);
        assert_eq!(s.get(1).unwrap().task, "fix flaky e2e", "sticky through the turn");
        // A new prompt replaces it.
        s.apply(payload_with_task(1, Status::Running, "now migrate the schema"), 4);
        assert_eq!(s.get(1).unwrap().task, "now migrate the schema");
    }

    #[test]
    fn idle_clears_the_task_like_it_clears_the_message() {
        // `/clear` fires SessionStart{clear} → an idle broadcast; the row must
        // fully recede — stale task text is as bad as a stale msg.
        let mut s = StatusStore::default();
        s.apply(payload_with_task(1, Status::Running, "old work"), 1);
        s.apply(payload(1, Status::Idle), 2);
        assert_eq!(s.get(1).unwrap().task, "", "idle resets the task");
    }

    #[test]
    fn clear_on_prompt_return_drops_the_task() {
        let mut s = StatusStore::default();
        s.apply(payload_with_task(1, Status::Done, "shipped thing"), 1);
        assert!(s.clear_on_prompt_return(1, 5));
        assert_eq!(s.get(1).unwrap().task, "", "producer gone — label gone");
    }

    #[test]
    fn apply_sets_last_change_tick_only_on_status_change() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 5, 0);
        assert_eq!(s.get(1).unwrap().last_change_tick, 5);
        s.apply(payload(1, Status::Running), 9, 0); // same status
        assert_eq!(s.get(1).unwrap().last_change_tick, 5);
        s.apply(payload(1, Status::Done), 12, 0); // changed
        assert_eq!(s.get(1).unwrap().last_change_tick, 12);
        // verify repo, branch, msg fields are set
        assert_eq!(s.get(1).unwrap().repo, "r");
        assert_eq!(s.get(1).unwrap().branch, "b");
        assert_eq!(s.get(1).unwrap().msg, "m");
    }

    #[test]
    fn clear_on_prompt_return_clears_terminal_but_not_running() {
        let mut s = StatusStore::default();
        // Done → cleared to Idle (agent exited after finishing), repo kept.
        let done = payload(1, Status::Done);
        s.apply(done, 1, 0);
        assert!(s.clear_on_prompt_return(1, 5).is_some());
        assert_eq!(s.get(1).unwrap().status, Status::Idle);
        assert_eq!(s.get(1).unwrap().msg, "", "message dropped");
        assert_eq!(s.get(1).unwrap().repo, "r", "repo kept so the tab keeps its name");

        // Error and Pending also clear (the producer is gone).
        s.apply(payload(2, Status::Error), 1, 0);
        s.apply(payload(3, Status::Pending), 1, 0);
        assert!(s.clear_on_prompt_return(2, 6).is_some());
        assert!(s.clear_on_prompt_return(3, 6).is_some());
        assert_eq!(s.get(2).unwrap().status, Status::Idle);
        assert_eq!(s.get(3).unwrap().status, Status::Idle);

        // Running is NOT cleared — a live turn's foreground flicker to a shell
        // must not be mistaken for the agent exiting.
        s.apply(payload(4, Status::Running), 1, 0);
        assert!(s.clear_on_prompt_return(4, 7).is_none());
        assert_eq!(s.get(4).unwrap().status, Status::Running);

        // Already-idle and unknown panes are no-ops (never panic).
        s.apply(payload(5, Status::Idle), 1, 0);
        assert!(s.clear_on_prompt_return(5, 8).is_none());
        assert!(s.clear_on_prompt_return(999, 8).is_none());
    }

    #[test]
    fn prune_removes_dead_panes() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1, 0);
        s.apply(payload(2, Status::Done), 1, 0);
        let live: HashSet<u32> = [2].into_iter().collect();
        s.prune(&live);
        assert!(s.get(1).is_none());
        assert!(s.get(2).is_some());
    }

    #[test]
    fn prune_returns_only_dropped_completions() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1, 0); // dropped, but not a completion
        s.apply(payload(2, Status::Done), 1, 100);
        s.apply(payload(3, Status::Idle), 1, 0); // dropped, but not a completion
        let live: HashSet<u32> = HashSet::new();
        let dropped = s.prune(&live);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].0, 2);
        assert_eq!(dropped[0].1.status, Status::Done);
    }

    #[test]
    fn ever_active_sticks_after_returning_to_idle() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1, 0);
        s.apply(payload(1, Status::Idle), 2, 0);
        assert!(s.get(1).unwrap().ever_active);
        assert!(!s.any_running());
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
        s.apply(done, 1, 100);
        assert_eq!(s.get(1).unwrap().msg, "shipped the feature");
        assert_eq!(s.get(1).unwrap().status, Status::Done);

        let mut idle = payload(1, Status::Idle);
        idle.msg = String::new();
        // The overwrite differs in status → the old Done recedes out.
        let receded = s.apply(idle, 2, 200);

        assert_eq!(s.get(1).unwrap().status, Status::Idle);
        assert_eq!(s.get(1).unwrap().msg, "", "stale message is cleared");
        assert_eq!(receded.unwrap().status, Status::Done, "the /clear overwrite is a recede edge");
    }

    #[test]
    fn apply_stamps_completion_epoch_on_the_edge_only() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1, 100);
        assert_eq!(s.get(1).unwrap().completed_epoch_s, None);
        s.apply(payload(1, Status::Done), 2, 200);
        assert_eq!(s.get(1).unwrap().completed_epoch_s, Some(200));
        s.apply(payload(1, Status::Done), 3, 300); // identical re-broadcast
        assert_eq!(s.get(1).unwrap().completed_epoch_s, Some(200), "no re-stamp");
    }

    #[test]
    fn apply_returns_displaced_completion_on_a_real_change_only() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Done), 1, 100);
        assert!(
            s.apply(payload(1, Status::Done), 2, 200).is_none(),
            "identical (status,msg) is a no-op edge"
        );
        let mut new_msg = payload(1, Status::Done);
        new_msg.msg = "another turn".into();
        let displaced = s.apply(new_msg, 3, 300);
        assert_eq!(displaced.unwrap().completed_epoch_s, Some(100), "old completion comes out");
        assert_eq!(s.get(1).unwrap().completed_epoch_s, Some(300), "new one stamped fresh");
        // A non-completion overwrite (Running) still displaces the old Done:
        s.apply(payload(2, Status::Done), 1, 100);
        let displaced = s.apply(payload(2, Status::Running), 2, 200);
        assert_eq!(displaced.unwrap().status, Status::Done);
    }

    #[test]
    fn clear_on_prompt_return_hands_back_the_completion() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Done), 1, 100);
        let old = s.clear_on_prompt_return(1, 5).expect("cleared");
        assert_eq!(old.status, Status::Done);
        assert_eq!(old.completed_epoch_s, Some(100));
        assert!(s.clear_on_prompt_return(1, 6).is_none(), "already idle");
    }
}
