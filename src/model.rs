//! Aggregate per-pane state into per-tab state. No zellij-tile dependency.

use crate::command;
use crate::kind::Kind;
use crate::state::StateStore;
use crate::status::Status;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub since_tick: u64,
    pub status: Status,
    /// Source-agnostic kind of the winning pane (agent or task type).
    pub kind: Kind,
}

/// One ever-active pane's per-pane state, the unit the multi-pane adaptive tree
/// renders as a child line. Built in `aggregate()` for every ever-active pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneEntry {
    pub pane_id: u32,
    pub kind: Kind,
    pub status: Status,
    pub msg: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabAgg {
    pub status: Status,
    pub done: usize,
    pub total: usize,
    pub pending: usize,
    pub detail: Option<Detail>,
    /// Per-pane entries for each ever-active pane in the tab, in pane-id
    /// iteration order. Empty for single-agent or plain (non-multi) tabs.
    /// `panes.len() > 1` is the "multi-pane" condition for the tree.
    pub panes: Vec<PaneEntry>,
}

/// Highest-severity pane wins (tie → most recent last_change_tick). `total`
/// counts panes that have ever been active and still exist; `done` counts
/// those currently done. Agent state (from `store`) takes precedence over
/// command activity (from `commands`) for any pane present in both.
pub fn aggregate(pane_ids: &[u32], store: &StateStore, commands: &command::CommandStore) -> TabAgg {
    let mut best_status = Status::Idle;
    let mut best: Option<Detail> = None;
    let mut done = 0usize;
    let mut total = 0usize;
    let mut pending = 0usize;
    let mut panes: Vec<PaneEntry> = Vec::new();

    for &id in pane_ids {
        let Some(s) = store.get(id).or_else(|| commands.get(id)) else { continue };
        if s.ever_active {
            total += 1;
            panes.push(PaneEntry {
                pane_id: id,
                kind: Kind::from_source(&s.source),
                status: s.status,
                msg: s.msg.clone(),
            });
            if s.status == Status::Done {
                done += 1;
            }
        }
        if s.status == Status::Pending {
            pending += 1;
        }
        let better = s.status.severity() > best_status.severity()
            || (s.status.severity() == best_status.severity()
                && best.as_ref().is_none_or(|d| s.last_change_tick >= d.since_tick));
        if s.status.is_active() && better {
            best_status = s.status;
            best = Some(Detail {
                repo: s.repo.clone(),
                branch: s.branch.clone(),
                msg: s.msg.clone(),
                since_tick: s.last_change_tick,
                status: s.status,
                kind: Kind::from_source(&s.source),
            });
        }
    }

    TabAgg {
        status: best_status,
        done,
        total,
        pending,
        detail: best,
        panes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::StatusPayload;

    fn put(store: &mut StateStore, id: u32, status: Status, tick: u64, repo: &str) {
        store.apply(
            StatusPayload {
                pane_id: id,
                status,
                repo: repo.into(),
                branch: "b".into(),
                msg: "m".into(),
                on_focus: None,
                seq: None,
                source: "claude".into(),
            },
            tick,
        );
    }

    #[test]
    fn empty_tab_is_idle() {
        let store = StateStore::default();
        let agg = aggregate(&[1, 2], &store, &command::CommandStore::default());
        assert_eq!(agg.status, Status::Idle);
        assert_eq!(agg.total, 0);
        assert!(agg.detail.is_none());
    }

    #[test]
    fn highest_severity_wins_for_status_and_detail() {
        let mut store = StateStore::default();
        put(&mut store, 1, Status::Done, 1, "done-repo");
        put(&mut store, 2, Status::Pending, 2, "pending-repo");
        put(&mut store, 3, Status::Running, 3, "running-repo");
        let agg = aggregate(&[1, 2, 3], &store, &command::CommandStore::default());
        assert_eq!(agg.status, Status::Pending); // error>pending>running>done
        assert_eq!(agg.detail.unwrap().repo, "pending-repo");
    }

    #[test]
    fn counts_done_over_total_ever_active() {
        let mut store = StateStore::default();
        put(&mut store, 1, Status::Done, 1, "a");
        put(&mut store, 2, Status::Done, 1, "b");
        put(&mut store, 3, Status::Running, 1, "c");
        let agg = aggregate(&[1, 2, 3], &store, &command::CommandStore::default());
        assert_eq!(agg.done, 2);
        assert_eq!(agg.total, 3);
    }

    #[test]
    fn pending_count_matches_pending_panes() {
        let mut store = StateStore::default();
        put(&mut store, 1, Status::Pending, 1, "a");
        put(&mut store, 2, Status::Pending, 2, "b");
        put(&mut store, 3, Status::Running, 3, "c");
        let agg = aggregate(&[1, 2, 3], &store, &command::CommandStore::default());
        assert_eq!(agg.pending, 2);
    }

    #[test]
    fn severity_tie_breaks_on_most_recent_change() {
        let mut store = StateStore::default();
        put(&mut store, 1, Status::Running, 5, "older");
        put(&mut store, 2, Status::Running, 9, "newer");
        let agg = aggregate(&[1, 2], &store, &command::CommandStore::default());
        assert_eq!(agg.detail.unwrap().repo, "newer");
    }

    #[test]
    fn aggregate_populates_panes_per_ever_active_pane() {
        // Three ever-active panes → `panes` has 3 entries carrying pane_id,
        // kind, status and msg, in pane-id iteration order.
        let mut store = StateStore::default();
        put(&mut store, 1, Status::Running, 1, "a");
        put(&mut store, 2, Status::Done, 2, "b");
        put(&mut store, 3, Status::Pending, 3, "c");
        let agg = aggregate(&[1, 2, 3], &store, &command::CommandStore::default());
        assert_eq!(agg.panes.len(), 3);
        // Iteration order is pane-id order.
        assert_eq!(agg.panes[0].pane_id, 1);
        assert_eq!(agg.panes[0].status, Status::Running);
        assert_eq!(agg.panes[1].pane_id, 2);
        assert_eq!(agg.panes[1].status, Status::Done);
        assert_eq!(agg.panes[2].pane_id, 3);
        assert_eq!(agg.panes[2].status, Status::Pending);
        // Each entry carries the source-agnostic kind ("claude" from put()).
        assert_eq!(agg.panes[0].kind, Kind::Claude);
        // The msg is carried per-pane.
        assert_eq!(agg.panes[0].msg, "m");

        // A single idle pane (never active) → empty panes.
        let store2 = StateStore::default();
        let agg2 = aggregate(&[10], &store2, &command::CommandStore::default());
        assert!(agg2.panes.is_empty());
    }

    #[test]
    fn agent_takes_precedence_over_command_for_same_pane() {
        // Pane 1 is present in both StateStore (Running, repo "agent") and
        // CommandStore (Running, repo "cmd"). Agent state must win.
        let mut store = StateStore::default();
        put(&mut store, 1, Status::Running, 1, "agent");

        let mut commands = command::CommandStore::default();
        commands.on_command_changed(1, &["cargo".to_string(), "build".to_string()], true, Some("/work/cmd"), 1);
        commands.on_timer(2); // promote to Running with repo "cmd"

        let agg = aggregate(&[1], &store, &commands);
        assert_eq!(agg.detail.unwrap().repo, "agent", "agent state must win over command state");
    }

    #[test]
    fn command_only_pane_contributes_to_panes_and_total() {
        // Pane 1 is only in CommandStore (Running); pane 2 is only in StateStore (Done).
        // Both must appear in the aggregation.
        let mut store = StateStore::default();
        put(&mut store, 2, Status::Done, 1, "agent-done");

        let mut commands = command::CommandStore::default();
        commands.on_command_changed(1, &["vim".to_string()], true, Some("/work/cmd"), 1);
        commands.on_timer(2); // promote to Running with repo "cmd"

        let agg = aggregate(&[1, 2], &store, &commands);
        assert_eq!(agg.total, 2, "both agent-done and command-running count toward total");
        assert_eq!(agg.done, 1, "only the agent-done pane is Done");
        assert_eq!(agg.panes.len(), 2);
        assert!(agg.panes.iter().any(|p| p.status == Status::Running));
        assert!(agg.panes.iter().any(|p| p.status == Status::Done));
        // Running wins over Done in severity → detail comes from command pane
        assert_eq!(agg.detail.unwrap().repo, "cmd");
    }
}