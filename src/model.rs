//! Aggregate per-pane state into per-tab state. No zellij-tile dependency.

use crate::state::StateStore;
use crate::status::Status;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub repo: String,
    pub branch: String,
    pub msg: String,
    pub since_tick: u64,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabAgg {
    pub status: Status,
    pub done: usize,
    pub total: usize,
    pub pending: usize,
    pub detail: Option<Detail>,
}

/// Highest-severity pane wins (tie → most recent last_change_tick). `total`
/// counts panes that have ever been active and still exist; `done` counts
/// those currently done.
pub fn aggregate(pane_ids: &[u32], store: &StateStore) -> TabAgg {
    let mut best_status = Status::Idle;
    let mut best: Option<Detail> = None;
    let mut done = 0usize;
    let mut total = 0usize;
    let mut pending = 0usize;

    for &id in pane_ids {
        let Some(s) = store.get(id) else { continue };
        if s.ever_active {
            total += 1;
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
            });
        }
    }

    TabAgg {
        status: best_status,
        done,
        total,
        pending,
        detail: best,
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
                source: "test".into(),
            },
            tick,
        );
    }

    #[test]
    fn empty_tab_is_idle() {
        let store = StateStore::default();
        let agg = aggregate(&[1, 2], &store);
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
        let agg = aggregate(&[1, 2, 3], &store);
        assert_eq!(agg.status, Status::Pending); // error>pending>running>done
        assert_eq!(agg.detail.unwrap().repo, "pending-repo");
    }

    #[test]
    fn counts_done_over_total_ever_active() {
        let mut store = StateStore::default();
        put(&mut store, 1, Status::Done, 1, "a");
        put(&mut store, 2, Status::Done, 1, "b");
        put(&mut store, 3, Status::Running, 1, "c");
        let agg = aggregate(&[1, 2, 3], &store);
        assert_eq!(agg.done, 2);
        assert_eq!(agg.total, 3);
    }

    #[test]
    fn pending_count_matches_pending_panes() {
        let mut store = StateStore::default();
        put(&mut store, 1, Status::Pending, 1, "a");
        put(&mut store, 2, Status::Pending, 2, "b");
        put(&mut store, 3, Status::Running, 3, "c");
        let agg = aggregate(&[1, 2, 3], &store);
        assert_eq!(agg.pending, 2);
    }

    #[test]
    fn severity_tie_breaks_on_most_recent_change() {
        let mut store = StateStore::default();
        put(&mut store, 1, Status::Running, 5, "older");
        put(&mut store, 2, Status::Running, 9, "newer");
        let agg = aggregate(&[1, 2], &store);
        assert_eq!(agg.detail.unwrap().repo, "newer");
    }
}
