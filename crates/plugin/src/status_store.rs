//! Per-pane status-payload observations, keyed by terminal pane id.
//! No zellij-tile dependency.

use crate::kind::Kind;
use crate::observation::{ObservationOrigin, ObservationStore, TrackedObservation};
use crate::payload::StatusPayload;
use crate::status::Status;
use std::collections::{HashMap, HashSet};

/// Ticks a `Running` pane may sit at a shell prompt before its pushed status is
/// declared stale and cleared to idle (~seconds at the Fast cadence, which a
/// Running row always keeps armed). Long enough that a mid-turn foreground
/// flicker — which re-asserts the agent's foreground or a fresh hook payload
/// well inside the window — never trips it; short enough that killing an agent
/// mid-turn doesn't leave a "working" row spinning forever.
pub const RUNNING_SUSPECT_GRACE_TICKS: u64 = 15;

/// Upper bound on tracked status observations across distinct pane ids. The
/// per-payload defenses (size cap, sanitize) don't bound the number of
/// *panes*: a looping producer broadcasting fresh pane ids would otherwise
/// grow this store, the persisted snapshot, and the per-payload disk write
/// without limit — and a fresh instance that hasn't seen a `PaneUpdate` yet
/// never prunes. 256 is far beyond any legitimate session (one agent pane per
/// rail row) while keeping the worst-case store/snapshot small. On overflow
/// the oldest observation by `last_change_tick` is evicted. Deliberately NOT
/// a live-pane check: a legit broadcast can arrive before the `PaneUpdate`
/// that introduces its pane, so unknown ids must stay accepted.
pub const MAX_TRACKED_PANES: usize = 256;

#[derive(Default)]
pub struct StatusStore {
    store: ObservationStore,
    /// Grace clocks for panes whose foreground returned to a shell while their
    /// pushed status was still `Running` (pane id → tick first seen). A fresh
    /// payload or the agent's foreground reappearing cancels the clock; the
    /// timer expires it (`expire_stale_running`). Deliberately not persisted:
    /// suspicion is per-instance evidence, and the instance that saw the
    /// prompt return clears the shared snapshot for everyone.
    suspect_running: HashMap<u32, u64>,
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
        // Any payload is proof the producer is alive — cancel a pending
        // stale-Running grace clock before it can misfire.
        self.suspect_running.remove(&p.pane_id);
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
        // Mirror of the completion stamp for the waiting-on-you edge: kept
        // across identical re-broadcasts (the wait started at the FIRST ask),
        // re-stamped when a different question arrives (that wait is new).
        let pending_epoch_s = match p.status {
            Status::Pending if identical => prev.and_then(|s| s.pending_epoch_s),
            Status::Pending => Some(now_epoch_s),
            _ => None,
        };
        let was_completion = prev.is_some_and(|s| s.status.is_completion());
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
                pending_epoch_s,
            },
        );
        self.evict_over_cap();
        if identical || !was_completion {
            None
        } else {
            displaced
        }
    }

    /// Enforce [`MAX_TRACKED_PANES`] after an intake insert: while over the
    /// cap, drop the oldest observation by `last_change_tick` (ties broken by
    /// pane id, for determinism). An evicted completion is dropped silently —
    /// no ledger hand-off — because the cap only ever bites under a
    /// misbehaving producer, where ledgering its flood would be its own DoS.
    ///
    /// Known divergence: the shared snapshot keeps the evicted entry. The
    /// evicted pane is still live, so `snapshot::to_json`'s existing-entry
    /// filter never scrubs it, and no persist from here could either — the
    /// merge has no way to say "deliberately forgotten" for a live pane. The
    /// pane's next broadcast overwrites it; until then a freshly spawned
    /// instance may seed one flood-stale row. Accepted: reaching this path at
    /// all means a producer is flooding past the cap.
    fn evict_over_cap(&mut self) {
        while self.store.observations().count() > MAX_TRACKED_PANES {
            let Some(oldest) = self
                .store
                .observations()
                .min_by_key(|(id, o)| (o.last_change_tick, *id))
                .map(|(id, _)| id)
            else {
                return;
            };
            let keep: HashSet<u32> = self
                .store
                .observations()
                .map(|(id, _)| id)
                .filter(|&id| id != oldest)
                .collect();
            let _ = self.store.prune(&keep);
            self.suspect_running.remove(&oldest);
        }
    }

    /// Clear a pane's pushed status to idle because its producer is gone — the
    /// pane returned to a shell prompt (`command::is_shell_prompt`). No-op if the
    /// pane isn't tracked or is already Idle. A `Running` status is not cleared
    /// here either — a live agent turn re-asserts Running via its hooks, so a
    /// transient foreground flicker to a shell must never be mistaken for the
    /// agent exiting — but it *starts the grace clock*: if no payload or agent
    /// foreground vouches for the pane within `RUNNING_SUSPECT_GRACE_TICKS`,
    /// `expire_stale_running` clears it (the agent was killed mid-turn; no hook
    /// fires on a kill). Keeps repo/branch so the tab keeps its name; drops the
    /// message. Returns the observation this cleared (carrying its completion +
    /// stamp, if any) — the ledger's recede edge for "the agent exited after
    /// finishing." Unlike the (removed) focus-driven recede, this rides the
    /// shared `CommandChanged` signal, so every tab's instance clears in
    /// lockstep.
    pub fn clear_on_prompt_return(&mut self, pane_id: u32, tick: u64) -> Option<TrackedObservation> {
        let prev = self.store.get(pane_id)?;
        if prev.status == Status::Running {
            // First sighting starts the clock; repeats don't reset it (the
            // pane *staying* at the prompt is exactly the stale evidence).
            self.suspect_running.entry(pane_id).or_insert(tick);
            return None;
        }
        if prev.status == Status::Idle {
            return None;
        }
        self.force_idle(pane_id, tick)
    }

    /// Live evidence the producer is running (its exe is the pane's foreground
    /// again after a flicker) — cancel the stale-Running grace clock.
    pub fn cancel_running_suspect(&mut self, pane_id: u32) {
        self.suspect_running.remove(&pane_id);
    }

    /// Expire grace clocks: any pane still `Running` whose prompt-return
    /// suspicion has outlived the grace window gets cleared to idle — its
    /// producer died mid-turn and will never send the clearing broadcast.
    /// Returns the cleared pane ids (Running is not a completion, so there is
    /// no ledger edge to hand back). Driven by the timer, which a Running row
    /// always keeps armed at the Fast cadence.
    pub fn expire_stale_running(&mut self, now_tick: u64) -> Vec<u32> {
        let due: Vec<u32> = self
            .suspect_running
            .iter()
            .filter(|&(_, &since)| now_tick.saturating_sub(since) >= RUNNING_SUSPECT_GRACE_TICKS)
            .map(|(&id, _)| id)
            .collect();
        let mut cleared = Vec::new();
        for pane_id in due {
            self.suspect_running.remove(&pane_id);
            let still_running = self.store.get(pane_id).is_some_and(|s| s.status == Status::Running);
            if still_running && self.force_idle(pane_id, now_tick).is_some() {
                cleared.push(pane_id);
            }
        }
        cleared
    }

    /// The shared idle overwrite behind both clear paths: repo/branch/kind kept
    /// (the tab keeps its name), msg/task dropped, `ever_active` sticky.
    fn force_idle(&mut self, pane_id: u32, tick: u64) -> Option<TrackedObservation> {
        let old = self.store.get(pane_id)?.clone();
        let _ = self.store.insert(
            pane_id,
            TrackedObservation {
                origin: ObservationOrigin::StatusPipe,
                status: Status::Idle,
                repo: old.repo.clone(),
                branch: old.branch.clone(),
                msg: String::new(),
                task: String::new(),
                kind: old.kind,
                last_change_tick: tick,
                ever_active: true,
                exit_code: None,
                completed_epoch_s: None,
                pending_epoch_s: None,
            },
        );
        Some(old)
    }

    /// Prune panes no longer live, returning every dropped observation
    /// (`ObservationStore::prune`'s contract): the caller reads emptiness as
    /// "persisted state changed" and the ledger sink filters to the
    /// completions (a pane closing with an unreceded Done/Error is a recede
    /// edge; a dropped Running/Idle/Pending ledgers nothing but must still
    /// reach the snapshot).
    pub fn prune(&mut self, live: &HashSet<u32>) -> Vec<(u32, TrackedObservation)> {
        self.suspect_running.retain(|id, _| live.contains(id));
        self.store.prune(live)
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
        s.apply(payload_with_task(1, Status::Running, "fix flaky e2e"), 1, 0);
        assert_eq!(s.get(1).unwrap().task, "fix flaky e2e");
        // …PreToolUse / Stop broadcasts don't (empty task) — the label sticks.
        s.apply(payload(1, Status::Running), 2, 0);
        s.apply(payload(1, Status::Done), 3, 0);
        assert_eq!(s.get(1).unwrap().task, "fix flaky e2e", "sticky through the turn");
        // A new prompt replaces it.
        s.apply(payload_with_task(1, Status::Running, "now migrate the schema"), 4, 0);
        assert_eq!(s.get(1).unwrap().task, "now migrate the schema");
    }

    #[test]
    fn idle_clears_the_task_like_it_clears_the_message() {
        // `/clear` fires SessionStart{clear} → an idle broadcast; the row must
        // fully recede — stale task text is as bad as a stale msg.
        let mut s = StatusStore::default();
        s.apply(payload_with_task(1, Status::Running, "old work"), 1, 0);
        s.apply(payload(1, Status::Idle), 2, 0);
        assert_eq!(s.get(1).unwrap().task, "", "idle resets the task");
    }

    #[test]
    fn clear_on_prompt_return_drops_the_task() {
        let mut s = StatusStore::default();
        s.apply(payload_with_task(1, Status::Done, "shipped thing"), 1, 0);
        assert!(s.clear_on_prompt_return(1, 5).is_some());
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
    fn killed_mid_turn_running_clears_after_the_grace_window() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1, 0);
        // Prompt return while Running: not cleared — the grace clock starts.
        assert!(s.clear_on_prompt_return(1, 5).is_none());
        assert_eq!(s.get(1).unwrap().status, Status::Running);
        // Inside the window nothing expires.
        assert!(s.expire_stale_running(5 + RUNNING_SUSPECT_GRACE_TICKS - 1).is_empty());
        // At the window's edge the ghost clears: no hook ever fires on a kill.
        assert_eq!(s.expire_stale_running(5 + RUNNING_SUSPECT_GRACE_TICKS), vec![1]);
        assert_eq!(s.get(1).unwrap().status, Status::Idle);
        assert_eq!(s.get(1).unwrap().repo, "r", "repo kept so the tab keeps its name");
        assert!(s.get(1).unwrap().ever_active, "stays a muted row, not removed");
        assert!(!s.any_running(), "the ghost no longer pins the fast timer");
    }

    #[test]
    fn fresh_payload_or_agent_foreground_cancels_the_grace_clock() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1, 0);
        assert!(s.clear_on_prompt_return(1, 5).is_none());
        // A hook payload proves the producer is alive.
        s.apply(payload(1, Status::Running), 6, 0);
        assert!(s.expire_stale_running(5 + RUNNING_SUSPECT_GRACE_TICKS).is_empty());
        // Suspect again; this time the flicker resolves back to the agent's
        // foreground (`cancel_running_suspect`, wired from command_changed).
        assert!(s.clear_on_prompt_return(1, 40).is_none());
        s.cancel_running_suspect(1);
        assert!(s.expire_stale_running(40 + RUNNING_SUSPECT_GRACE_TICKS).is_empty());
        assert_eq!(s.get(1).unwrap().status, Status::Running);
    }

    #[test]
    fn repeated_prompt_returns_do_not_reset_the_grace_clock() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1, 0);
        assert!(s.clear_on_prompt_return(1, 5).is_none());
        // The pane *staying* at the prompt is the stale evidence — a repeat
        // sighting must not push expiry out.
        assert!(s.clear_on_prompt_return(1, 5 + RUNNING_SUSPECT_GRACE_TICKS - 1).is_none());
        assert_eq!(s.expire_stale_running(5 + RUNNING_SUSPECT_GRACE_TICKS), vec![1]);
    }

    #[test]
    fn pane_close_drops_its_grace_clock() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1, 0);
        assert!(s.clear_on_prompt_return(1, 5).is_none());
        s.prune(&HashSet::new());
        assert!(s.expire_stale_running(100).is_empty(), "no clock for a dead pane");
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
    fn prune_returns_every_drop_not_just_completions() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1, 0);
        s.apply(payload(2, Status::Done), 1, 100);
        s.apply(payload(3, Status::Idle), 1, 0);
        let live: HashSet<u32> = HashSet::new();
        let mut dropped = s.prune(&live);
        dropped.sort_by_key(|(id, _)| *id);
        // All three come back out: emptiness is the caller's persist signal,
        // and the ledger filters to the completion (pane 2) itself.
        assert_eq!(dropped.len(), 3);
        assert_eq!((dropped[0].0, dropped[0].1.status), (1, Status::Running));
        assert_eq!((dropped[1].0, dropped[1].1.status), (2, Status::Done));
        assert_eq!((dropped[2].0, dropped[2].1.status), (3, Status::Idle));
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
    fn apply_stamps_pending_epoch_on_the_edge_and_restamps_a_new_question() {
        let mut s = StatusStore::default();
        s.apply(payload(1, Status::Running), 1, 100);
        assert_eq!(s.get(1).unwrap().pending_epoch_s, None);
        // Edge into Pending stamps the wait's start.
        s.apply(payload(1, Status::Pending), 2, 200);
        assert_eq!(s.get(1).unwrap().pending_epoch_s, Some(200));
        // An identical re-broadcast keeps the ORIGINAL stamp — the wait
        // started at the first ask.
        s.apply(payload(1, Status::Pending), 3, 300);
        assert_eq!(s.get(1).unwrap().pending_epoch_s, Some(200));
        // A different question is a new wait.
        let mut new_q = payload(1, Status::Pending);
        new_q.msg = "another question?".into();
        s.apply(new_q, 4, 400);
        assert_eq!(s.get(1).unwrap().pending_epoch_s, Some(400));
        // Leaving Pending drops the stamp.
        s.apply(payload(1, Status::Running), 5, 500);
        assert_eq!(s.get(1).unwrap().pending_epoch_s, None);
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
    fn store_caps_tracked_panes_and_evicts_the_oldest() {
        // A looping producer sending ever-fresh pane ids must not grow the
        // store (and with it the persisted snapshot) without bound. Insert
        // cap+8 distinct panes, each at a later tick: the store holds exactly
        // the cap, and the 8 oldest-by-tick observations are the ones evicted.
        let mut s = StatusStore::default();
        let extra = 8u32;
        for i in 0..(MAX_TRACKED_PANES as u32 + extra) {
            s.apply(payload(i, Status::Running), i as u64, 0);
        }
        assert_eq!(s.observations().count(), MAX_TRACKED_PANES);
        for i in 0..extra {
            assert!(s.get(i).is_none(), "oldest pane {i} must be evicted");
        }
        assert!(s.get(extra).is_some(), "the oldest survivor is still tracked");
        assert!(
            s.get(MAX_TRACKED_PANES as u32 + extra - 1).is_some(),
            "the newest pane is still tracked"
        );
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
