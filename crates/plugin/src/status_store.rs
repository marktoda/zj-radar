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
        self.store.insert(
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
            },
        );
    }

    /// Clear a pane's pushed status to idle because its producer is gone — the
    /// pane returned to a shell prompt (`command::is_shell_prompt`). No-op if the
    /// pane isn't tracked, is already Idle, or is currently Running: a live agent
    /// turn re-asserts Running via its hooks, so a transient foreground flicker to
    /// a shell must never be mistaken for the agent exiting. Keeps repo/branch so
    /// the tab keeps its name; drops the message. Returns whether it changed
    /// anything. Unlike the (removed) focus-driven recede, this rides the shared
    /// `CommandChanged` signal, so every tab's instance clears in lockstep.
    pub fn clear_on_prompt_return(&mut self, pane_id: u32, tick: u64) -> bool {
        let Some(prev) = self.store.get(pane_id) else {
            return false;
        };
        if matches!(
            prev.status,
            crate::status::Status::Running | crate::status::Status::Idle
        ) {
            return false;
        }
        let repo = prev.repo.clone();
        let branch = prev.branch.clone();
        let kind = prev.kind;
        self.store.insert(
            pane_id,
            TrackedObservation {
                origin: ObservationOrigin::StatusPipe,
                status: crate::status::Status::Idle,
                repo,
                branch,
                msg: String::new(),
                task: String::new(),
                kind,
                last_change_tick: tick,
                ever_active: true,
                exit_code: None,
            },
        );
        true
    }

    pub fn prune(&mut self, live: &HashSet<u32>) {
        self.store.prune(live);
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
    fn clear_on_prompt_return_clears_terminal_but_not_running() {
        let mut s = StatusStore::default();
        // Done → cleared to Idle (agent exited after finishing), repo kept.
        let done = payload(1, Status::Done);
        s.apply(done, 1);
        assert!(s.clear_on_prompt_return(1, 5));
        assert_eq!(s.get(1).unwrap().status, Status::Idle);
        assert_eq!(s.get(1).unwrap().msg, "", "message dropped");
        assert_eq!(s.get(1).unwrap().repo, "r", "repo kept so the tab keeps its name");

        // Error and Pending also clear (the producer is gone).
        s.apply(payload(2, Status::Error), 1);
        s.apply(payload(3, Status::Pending), 1);
        assert!(s.clear_on_prompt_return(2, 6));
        assert!(s.clear_on_prompt_return(3, 6));
        assert_eq!(s.get(2).unwrap().status, Status::Idle);
        assert_eq!(s.get(3).unwrap().status, Status::Idle);

        // Running is NOT cleared — a live turn's foreground flicker to a shell
        // must not be mistaken for the agent exiting.
        s.apply(payload(4, Status::Running), 1);
        assert!(!s.clear_on_prompt_return(4, 7));
        assert_eq!(s.get(4).unwrap().status, Status::Running);

        // Already-idle and unknown panes are no-ops (never panic).
        s.apply(payload(5, Status::Idle), 1);
        assert!(!s.clear_on_prompt_return(5, 8));
        assert!(!s.clear_on_prompt_return(999, 8));
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
        s.apply(done, 1);
        assert_eq!(s.get(1).unwrap().msg, "shipped the feature");
        assert_eq!(s.get(1).unwrap().status, Status::Done);

        let mut idle = payload(1, Status::Idle);
        idle.msg = String::new();
        s.apply(idle, 2);

        assert_eq!(s.get(1).unwrap().status, Status::Idle);
        assert_eq!(s.get(1).unwrap().msg, "", "stale message is cleared");
    }
}
