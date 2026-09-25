//! Background tasks: work an agent started that runs without its attention (a
//! backgrounded test run, a background subagent, a dev server). Two halves,
//! one vocabulary:
//!
//! - the **wire** half ([`TaskUpdate`], [`TaskBatch`]) rides the optional
//!   `tasks` field of `zj_radar.status.v1` — see `payload.rs`;
//! - the **stored** half ([`BgTask`], [`BgTasks`]) is what the plugin keeps per
//!   pane inside `TrackedObservation`, folded by [`BgTasks::apply`] — the one
//!   merge rule, owned here so it is host-testable and never re-implemented.
//!
//! "Background task", not "job": the activity model already uses *Job* for
//! bounded work as opposed to a *Service*, and a background task can be either
//! (the `holds` flag is that split). No zellij-tile dependency.

use crate::payload::sanitize_in_place;
use crate::status::Status;
use crate::wire::wire_enum;
use serde::{Deserialize, Serialize};

/// Public-contract limit: a `tasks` batch keeps at most this many items (the
/// producer truncates in `to_wire`, `parse` truncates again); the plugin also
/// stores at most this many per pane.
pub const MAX_TASKS: usize = 16;
/// Public-contract limit: a task `id` truncates to this many chars.
pub const MAX_TASK_ID_CHARS: usize = 32;
/// Public-contract limit: a task `label` truncates to this many chars.
pub const MAX_TASK_LABEL_CHARS: usize = 64;

wire_enum! {
    /// Where a background task is in its life. Strict: an unknown token is a
    /// parse error, so the item is skipped rather than guessed — a guessed
    /// `completed` would paint a false green `●` over work that may still be
    /// running, or that failed.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum TaskState {
        Running => "running",
        Completed => "completed",
        Failed => "failed",
        Killed => "killed",
        /// Gone from a fresh snapshot with no reported outcome. Deliberately
        /// not `Completed`: a lost outcome must never read as success.
        Ended => "ended",
    }
}

impl TaskState {
    /// Every state but `Running` is over: nothing revives an ended task.
    pub fn is_ended(self) -> bool {
        self != TaskState::Running
    }

    /// A reported outcome — final. `Ended` is over but not final: a snapshot
    /// can end a task before its outcome arrives (it finished after the
    /// turn's last tool call, and the wake carrying `failed` lands after the
    /// `Stop`), and that late outcome must still replace the muted `·`.
    pub fn is_final(self) -> bool {
        matches!(self, TaskState::Completed | TaskState::Failed | TaskState::Killed)
    }
}

/// One background task as a producer reports it. Items are upserts by `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskUpdate {
    pub id: String,
    pub state: TaskState,
    /// Display label; empty = keep the stored one.
    pub label: String,
    /// Bounded work the agent will be woken by (it "holds" the agent). False
    /// for services (dev servers, followed logs, monitors), which may never end.
    pub holds: bool,
}

/// The `tasks` field: a batch of upserts, optionally an authoritative
/// snapshot. A snapshot is the complete *running* set: every stored running
/// task it omits has ended (with no outcome — [`TaskState::Ended`]). The
/// wire codec clears `snapshot` on a batch it truncates to [`MAX_TASKS`], so
/// a capped item is never mistaken for an omitted one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskBatch {
    pub items: Vec<TaskUpdate>,
    pub snapshot: bool,
}

impl TaskBatch {
    /// Does this batch carry a running task that holds — bounded work the
    /// agent will be woken by? (`BgTasks::apply` checks `snapshot` itself:
    /// only a turn-end snapshot makes the agent *wait*.)
    pub fn has_holding(&self) -> bool {
        self.items.iter().any(|t| t.state == TaskState::Running && t.holds)
    }
}

/// One stored background task. Persisted in the `/cache` snapshot, so ages
/// are wall-clock epoch seconds (ticks are per plugin instance and would
/// disagree across tabs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BgTask {
    pub id: String,
    pub label: String,
    pub holds: bool,
    pub state: TaskState,
    pub started_epoch_s: u64,
    #[serde(default)]
    pub ended_epoch_s: Option<u64>,
}

/// A pane's background tasks plus whether the agent is waiting on them.
/// Serde-defaulted inside `TrackedObservation`, so pre-task snapshots load.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BgTasks {
    #[serde(default, deserialize_with = "lenient_items")]
    pub items: Vec<BgTask>,
    /// The agent's turn is over and it is waiting on a holding task. Set per
    /// payload (a Running payload carrying a snapshot with a holding task),
    /// never inferred from leftover items: tasks outlive a human prompt, so a
    /// running task does not mean the agent is idle.
    #[serde(default)]
    pub waiting: bool,
}

/// Load stored items one at a time, skipping any that don't parse. The
/// snapshot is one shared document every instance merge-writes: a strict
/// item (say a `TaskState` a newer build added) would fail the whole load,
/// and the older instance would then rehydrate nothing and overwrite the
/// file with only its own panes. `Status` and `Kind` are lenient for the
/// same reason; a task has no safe fallback state, so it is dropped instead.
fn lenient_items<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<BgTask>, D::Error> {
    let raw = <Vec<serde_json::Value> as Deserialize>::deserialize(de)?;
    Ok(raw.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

impl BgTasks {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Serde skip predicate: omit the field from a snapshot record entirely
    /// when there is nothing to say, so records for the common (task-less)
    /// pane are byte-identical to before.
    pub fn is_empty_default(&self) -> bool {
        *self == BgTasks::default()
    }

    /// Any running task that holds the agent — bounded work still in flight.
    pub fn any_holding(&self) -> bool {
        self.items.iter().any(|t| t.state == TaskState::Running && t.holds)
    }

    /// Fold one payload into the stored list — the single merge rule.
    ///
    /// - `Idle` clears everything (`/clear`, `SessionEnd`).
    /// - A payload with no batch that starts a new batch of work clears
    ///   finished tasks (running ones stay): one that takes the pane from a
    ///   *completion* (`Done`/`Error`) to `Running`, or one carrying a fresh
    ///   sticky label (`prompted` — only a human `UserPromptSubmit` sets one;
    ///   that covers answering a question that ended the turn). A bare
    ///   Pending → Running doesn't: it is also the mid-turn recovery edge
    ///   after a permission answer, and a turn in flight keeps its summary.
    /// - A batch upserts by id; a reported outcome is final, and `Ended`
    ///   yields only to one; `holds` only ever drops; a snapshot ends every stored running task it
    ///   omits (→ `Ended`).
    /// - Stored items are capped at [`MAX_TASKS`], evicting ended tasks first
    ///   (oldest end first), then the oldest running.
    /// - `waiting` is recomputed from this payload alone.
    pub fn apply(&mut self, batch: Option<&TaskBatch>, prev: Option<Status>, next: Status, prompted: bool, now: u64) {
        if next == Status::Idle {
            *self = BgTasks::default();
            return;
        }
        match batch {
            None => {
                if next == Status::Running && (prompted || prev.is_some_and(Status::is_completion)) {
                    self.items.retain(|t| !t.state.is_ended());
                }
            }
            Some(batch) => {
                for update in &batch.items {
                    self.upsert(update, now);
                }
                if batch.snapshot {
                    for task in self.items.iter_mut().filter(|t| t.state == TaskState::Running) {
                        if !batch.items.iter().any(|u| u.id == task.id && u.state == TaskState::Running) {
                            task.state = TaskState::Ended;
                            task.ended_epoch_s = Some(now);
                        }
                    }
                }
            }
        }
        self.cap();
        self.waiting =
            next == Status::Running && batch.is_some_and(|b| b.snapshot && b.has_holding()) && self.any_holding();
    }

    fn upsert(&mut self, update: &TaskUpdate, now: u64) {
        let ended = update.state.is_ended().then_some(now);
        match self.items.iter_mut().find(|t| t.id == update.id) {
            Some(task) if task.state.is_final() => {}
            // Over with no outcome: only a reported outcome may replace it.
            Some(task) if task.state.is_ended() && !update.state.is_final() => {}
            Some(task) => {
                if !update.label.is_empty() {
                    task.label = update.label.clone();
                }
                // A running upsert may lower `holds`, never raise it: once a
                // task is known to be a service, a later report with less to
                // go on (a snapshot entry missing its command) must not make
                // the agent wait on it.
                if update.state == TaskState::Running {
                    task.holds &= update.holds;
                }
                // A late outcome for an `Ended` task keeps the end the
                // snapshot observed — the closer bound on when it finished.
                task.ended_epoch_s = if task.state.is_ended() { task.ended_epoch_s.or(ended) } else { ended };
                task.state = update.state;
            }
            None => self.items.push(BgTask {
                id: update.id.clone(),
                label: update.label.clone(),
                holds: update.holds,
                state: update.state,
                started_epoch_s: now,
                ended_epoch_s: ended,
            }),
        }
    }

    fn cap(&mut self) {
        while self.items.len() > MAX_TASKS {
            let victim = self
                .items
                .iter()
                .enumerate()
                .min_by_key(|(_, t)| (!t.state.is_ended(), t.ended_epoch_s.unwrap_or(t.started_epoch_s)))
                .map(|(i, _)| i)
                .expect("len > MAX_TASKS > 0");
            self.items.remove(victim);
        }
    }

    /// Re-scrub stored text for items loaded off disk (see
    /// `TrackedObservation::sanitized`).
    pub fn sanitize(&mut self) {
        for task in &mut self.items {
            sanitize_in_place(&mut task.id, MAX_TASK_ID_CHARS);
            sanitize_in_place(&mut task.label, MAX_TASK_LABEL_CHARS);
        }
        self.items.truncate(MAX_TASKS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn up(id: &str, state: TaskState, holds: bool) -> TaskUpdate {
        TaskUpdate { id: id.into(), state, label: format!("{id} label"), holds }
    }

    fn snapshot(items: Vec<TaskUpdate>) -> TaskBatch {
        TaskBatch { items, snapshot: true }
    }

    fn delta(items: Vec<TaskUpdate>) -> TaskBatch {
        TaskBatch { items, snapshot: false }
    }

    fn states(t: &BgTasks) -> Vec<(&str, TaskState)> {
        t.items.iter().map(|x| (x.id.as_str(), x.state)).collect()
    }

    #[test]
    fn state_wire_round_trips_and_rejects_unknown() {
        for &s in TaskState::ALL {
            assert_eq!(TaskState::from_wire(s.as_wire()), Some(s));
        }
        assert_eq!(TaskState::from_wire("event"), None);
    }

    #[test]
    fn a_turn_end_snapshot_with_holding_work_is_waiting() {
        let mut t = BgTasks::default();
        t.apply(
            Some(&snapshot(vec![up("b1", TaskState::Running, true)])),
            Some(Status::Running),
            Status::Running,
            false,
            100,
        );
        assert!(t.waiting);
        assert_eq!(t.items[0].started_epoch_s, 100);
        // Services alone don't make the agent wait.
        let mut s = BgTasks::default();
        s.apply(
            Some(&snapshot(vec![up("dev", TaskState::Running, false)])),
            Some(Status::Running),
            Status::Done,
            false,
            100,
        );
        assert!(!s.waiting);
        assert_eq!(states(&s), vec![("dev", TaskState::Running)]);
    }

    #[test]
    fn a_start_delta_stamps_the_real_start_and_the_snapshot_keeps_it() {
        let mut t = BgTasks::default();
        t.apply(
            Some(&delta(vec![up("b1", TaskState::Running, true)])),
            Some(Status::Running),
            Status::Running,
            false,
            10,
        );
        assert!(!t.waiting, "a mid-turn start is not a turn end");
        t.apply(
            Some(&snapshot(vec![up("b1", TaskState::Running, true)])),
            Some(Status::Running),
            Status::Running,
            false,
            90,
        );
        assert_eq!(t.items[0].started_epoch_s, 10);
        assert!(t.waiting);
    }

    #[test]
    fn an_outcome_is_terminal_and_a_snapshot_cannot_revive_it() {
        let mut t = BgTasks::default();
        t.apply(Some(&delta(vec![up("b1", TaskState::Running, true)])), None, Status::Running, false, 10);
        t.apply(
            Some(&delta(vec![up("b1", TaskState::Failed, false)])),
            Some(Status::Running),
            Status::Running,
            false,
            20,
        );
        assert_eq!(states(&t), vec![("b1", TaskState::Failed)]);
        assert_eq!(t.items[0].ended_epoch_s, Some(20));
        t.apply(
            Some(&snapshot(vec![up("b1", TaskState::Running, true)])),
            Some(Status::Running),
            Status::Running,
            false,
            30,
        );
        assert_eq!(states(&t), vec![("b1", TaskState::Failed)], "an outcome is final");
        t.apply(
            Some(&delta(vec![up("b1", TaskState::Completed, false)])),
            Some(Status::Running),
            Status::Running,
            false,
            40,
        );
        assert_eq!(states(&t), vec![("b1", TaskState::Failed)], "…even against another outcome");
        assert!(!t.waiting);
    }

    #[test]
    fn a_late_outcome_replaces_a_snapshot_ended_task() {
        // The test failed after the turn's last tool call: the Stop's
        // snapshot no longer lists it (→ Ended), then the wake says failed.
        let mut t = BgTasks::default();
        t.apply(Some(&delta(vec![up("b1", TaskState::Running, true)])), None, Status::Running, false, 10);
        t.apply(Some(&snapshot(vec![])), Some(Status::Running), Status::Done, false, 20);
        assert_eq!(states(&t), vec![("b1", TaskState::Ended)]);
        t.apply(Some(&delta(vec![up("b1", TaskState::Failed, false)])), Some(Status::Done), Status::Running, false, 25);
        assert_eq!(states(&t), vec![("b1", TaskState::Failed)]);
        assert_eq!(t.items[0].ended_epoch_s, Some(20), "keeps the end the snapshot saw");
        // …but a running upsert can't revive it.
        let mut r = BgTasks::default();
        r.apply(Some(&snapshot(vec![up("b2", TaskState::Running, true)])), None, Status::Running, false, 10);
        r.apply(Some(&snapshot(vec![])), Some(Status::Running), Status::Done, false, 20);
        r.apply(Some(&delta(vec![up("b2", TaskState::Running, true)])), Some(Status::Done), Status::Running, false, 30);
        assert_eq!(states(&r), vec![("b2", TaskState::Ended)]);
    }

    #[test]
    fn a_snapshot_ends_omitted_running_tasks_without_claiming_success() {
        let mut t = BgTasks::default();
        t.apply(
            Some(&snapshot(vec![up("b1", TaskState::Running, true), up("b2", TaskState::Running, true)])),
            None,
            Status::Running,
            false,
            10,
        );
        t.apply(
            Some(&snapshot(vec![up("b2", TaskState::Running, true)])),
            Some(Status::Running),
            Status::Running,
            false,
            20,
        );
        assert_eq!(states(&t), vec![("b1", TaskState::Ended), ("b2", TaskState::Running)]);
        t.apply(Some(&snapshot(vec![])), Some(Status::Running), Status::Done, false, 30);
        assert_eq!(states(&t), vec![("b1", TaskState::Ended), ("b2", TaskState::Ended)]);
        assert!(!t.waiting);
    }

    #[test]
    fn an_outcome_for_an_unseen_id_is_recorded() {
        // Started and finished inside one turn: no Stop ever listed it.
        let mut t = BgTasks::default();
        t.apply(
            Some(&delta(vec![up("x", TaskState::Completed, false)])),
            Some(Status::Running),
            Status::Running,
            false,
            5,
        );
        assert_eq!(states(&t), vec![("x", TaskState::Completed)]);
    }

    #[test]
    fn a_new_human_batch_clears_finished_tasks_but_keeps_running_ones() {
        let mut t = BgTasks::default();
        t.apply(
            Some(&delta(vec![up("dev", TaskState::Running, false), up("b1", TaskState::Completed, false)])),
            None,
            Status::Done,
            false,
            5,
        );
        // Done → Running with no batch: the user prompted again.
        t.apply(None, Some(Status::Done), Status::Running, false, 6);
        assert_eq!(states(&t), vec![("dev", TaskState::Running)]);
        // Error → Running clears too.
        t.apply(Some(&delta(vec![up("b2", TaskState::Killed, false)])), Some(Status::Running), Status::Error, false, 7);
        t.apply(None, Some(Status::Error), Status::Running, false, 8);
        assert_eq!(states(&t), vec![("dev", TaskState::Running)]);
        // Pending → Running does NOT: it is also the mid-turn permission
        // answer's recovery edge, and a turn in flight keeps its summary.
        t.apply(
            Some(&delta(vec![up("b4", TaskState::Failed, false)])),
            Some(Status::Running),
            Status::Pending,
            false,
            8,
        );
        t.apply(None, Some(Status::Pending), Status::Running, false, 9);
        assert!(states(&t).contains(&("b4", TaskState::Failed)));
        // …but a real prompt answering it (a fresh sticky label) does.
        t.apply(
            Some(&delta(vec![up("b5", TaskState::Completed, false)])),
            Some(Status::Running),
            Status::Pending,
            false,
            9,
        );
        t.apply(None, Some(Status::Pending), Status::Running, true, 9);
        assert!(!states(&t).iter().any(|(id, _)| *id == "b5" || *id == "b4"));
        t.apply(
            Some(&delta(vec![up("b4", TaskState::Failed, false)])),
            Some(Status::Running),
            Status::Running,
            false,
            9,
        );
        // Running → Running (prompting while waiting) keeps the summary.
        t.apply(
            Some(&delta(vec![up("b3", TaskState::Completed, false)])),
            Some(Status::Running),
            Status::Running,
            false,
            9,
        );
        t.apply(None, Some(Status::Running), Status::Running, false, 10);
        assert_eq!(states(&t).len(), 3, "dev, b4, b3 all kept");
    }

    #[test]
    fn a_task_bearing_wake_is_not_a_new_human_batch() {
        // A service dying while the agent is Done wakes it with its outcome:
        // that must show, not be cleared by the Done → Running edge.
        let mut t = BgTasks::default();
        t.apply(Some(&snapshot(vec![up("dev", TaskState::Running, false)])), None, Status::Done, false, 5);
        t.apply(Some(&delta(vec![up("dev", TaskState::Failed, false)])), Some(Status::Done), Status::Running, false, 6);
        assert_eq!(states(&t), vec![("dev", TaskState::Failed)]);
    }

    #[test]
    fn idle_clears_everything() {
        let mut t = BgTasks::default();
        t.apply(Some(&snapshot(vec![up("b1", TaskState::Running, true)])), None, Status::Running, false, 5);
        t.apply(None, Some(Status::Running), Status::Idle, false, 6);
        assert_eq!(t, BgTasks::default());
    }

    #[test]
    fn labels_update_when_given_and_persist_when_blank() {
        let mut t = BgTasks::default();
        t.apply(Some(&delta(vec![up("b1", TaskState::Running, true)])), None, Status::Running, false, 5);
        let blank = TaskUpdate { id: "b1".into(), state: TaskState::Completed, label: String::new(), holds: false };
        t.apply(Some(&delta(vec![blank])), Some(Status::Running), Status::Running, false, 6);
        assert_eq!(t.items[0].label, "b1 label");
        assert!(t.items[0].holds, "an outcome doesn't rewrite holds");
    }

    #[test]
    fn a_running_upsert_lowers_holds_but_never_raises_it() {
        // Started as a dev server (holds=false); a turn-end snapshot that
        // lost the command must not make the agent wait on it.
        let mut t = BgTasks::default();
        t.apply(Some(&delta(vec![up("dev", TaskState::Running, false)])), None, Status::Running, false, 5);
        t.apply(
            Some(&snapshot(vec![up("dev", TaskState::Running, true)])),
            Some(Status::Running),
            Status::Running,
            false,
            6,
        );
        assert!(!t.items[0].holds);
        assert!(!t.waiting, "nothing bounded to wait on");
        // …while a bounded task a later report calls a service stops holding.
        let mut b = BgTasks::default();
        b.apply(Some(&delta(vec![up("b1", TaskState::Running, true)])), None, Status::Running, false, 5);
        b.apply(
            Some(&snapshot(vec![up("b1", TaskState::Running, false)])),
            Some(Status::Running),
            Status::Done,
            false,
            6,
        );
        assert!(!b.items[0].holds);
    }

    #[test]
    fn the_stored_list_is_capped_evicting_ended_first() {
        let mut t = BgTasks::default();
        let many: Vec<_> = (0..MAX_TASKS).map(|i| up(&format!("r{i}"), TaskState::Running, true)).collect();
        t.apply(Some(&delta(many)), None, Status::Running, false, 5);
        t.apply(
            Some(&delta(vec![up("old", TaskState::Completed, false)])),
            Some(Status::Running),
            Status::Running,
            false,
            6,
        );
        t.apply(
            Some(&delta(vec![up("new", TaskState::Running, true)])),
            Some(Status::Running),
            Status::Running,
            false,
            7,
        );
        assert_eq!(t.items.len(), MAX_TASKS);
        assert!(t.items.iter().all(|x| x.id != "old"), "the ended task went first");
        assert!(t.items.iter().any(|x| x.id == "new"));
    }

    #[test]
    fn a_stored_item_this_build_cannot_read_is_skipped_not_fatal() {
        // A newer build's state token must not fail the whole shared snapshot.
        let json = r#"{"items":[{"id":"a","label":"","holds":true,"state":"paused","started_epoch_s":1},
            {"id":"b","label":"x","holds":false,"state":"failed","started_epoch_s":1,"ended_epoch_s":2},"junk"],"waiting":false}"#;
        let t: BgTasks = serde_json::from_str(json).unwrap();
        assert_eq!(states(&t), vec![("b", TaskState::Failed)]);
    }

    #[test]
    fn stored_tasks_round_trip_and_old_records_load() {
        let mut t = BgTasks::default();
        t.apply(Some(&snapshot(vec![up("b1", TaskState::Running, true)])), None, Status::Running, false, 5);
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains(r#""state":"running""#), "wire tokens persist: {json}");
        assert_eq!(serde_json::from_str::<BgTasks>(&json).unwrap(), t);
        assert_eq!(serde_json::from_str::<BgTasks>("{}").unwrap(), BgTasks::default());
    }

    fn arb_update() -> impl Strategy<Value = TaskUpdate> {
        ("[a-d]", proptest::sample::select(TaskState::ALL.to_vec()), any::<bool>())
            .prop_map(|(id, state, holds)| TaskUpdate { id, state, label: String::new(), holds })
    }

    fn arb_step() -> impl Strategy<Value = (Option<TaskBatch>, Status)> {
        (
            proptest::option::of(
                (proptest::collection::vec(arb_update(), 0..5), any::<bool>())
                    .prop_map(|(items, snapshot)| TaskBatch { items, snapshot }),
            ),
            proptest::sample::select(Status::ALL.to_vec()),
        )
    }

    proptest! {
        /// The merge invariants, over arbitrary event sequences: never more
        /// than MAX_TASKS; nothing ended is revived and a reported outcome
        /// never changes; nothing survives Idle; waiting implies a Running
        /// row with a holding task.
        #[test]
        fn merge_invariants_hold(steps in proptest::collection::vec(arb_step(), 0..30)) {
            let mut t = BgTasks::default();
            let mut prev: Option<Status> = None;
            for (now, (batch, next)) in steps.into_iter().enumerate() {
                let ended_before: Vec<(String, TaskState)> = t.items.iter()
                    .filter(|x| x.state.is_ended()).map(|x| (x.id.clone(), x.state)).collect();
                t.apply(batch.as_ref(), prev, next, false, now as u64);
                prop_assert!(t.items.len() <= MAX_TASKS);
                if next == Status::Idle {
                    prop_assert_eq!(&t, &BgTasks::default());
                }
                for (id, state) in &ended_before {
                    if let Some(x) = t.items.iter().find(|x| &x.id == id) {
                        prop_assert!(x.state.is_ended(), "ended {} was revived", id);
                        if state.is_final() {
                            prop_assert_eq!(x.state, *state, "final outcome of {} changed", id);
                        }
                    }
                }
                if t.waiting {
                    prop_assert_eq!(next, Status::Running);
                    prop_assert!(t.any_holding());
                }
                prev = Some(next);
            }
        }
    }
}
