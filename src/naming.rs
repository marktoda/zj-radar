//! Pure tab-naming logic. No zellij-tile dependency.

use crate::state::StateStore;
use std::collections::HashMap;

/// Display-relevant subset of a terminal pane (from PaneInfo).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneLite {
    pub id: u32,
    pub title: String,
    pub is_focused: bool,
}

/// True if `name` is a Zellij default tab name like "Tab #1".
pub fn is_default_name(name: &str) -> bool {
    name.strip_prefix("Tab #")
        .map_or(false, |rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// Desired display name for one tab, or None if no push signal is available.
/// Agent repo (focused pane first, then any) wins; else the focused (then first)
/// pane's title.
pub fn computed_name(panes: &[PaneLite], store: &StateStore) -> Option<String> {
    let repo_of = |p: &PaneLite| {
        store
            .get(p.id)
            .map(|s| s.repo.clone())
            .filter(|r| !r.is_empty())
    };
    let focused = panes.iter().find(|p| p.is_focused);
    if let Some(p) = focused {
        if let Some(r) = repo_of(p) {
            return Some(r);
        }
    }
    for p in panes {
        if let Some(r) = repo_of(p) {
            return Some(r);
        }
    }
    if let Some(p) = focused {
        if !p.title.is_empty() {
            return Some(p.title.clone());
        }
    }
    if let Some(p) = panes.first() {
        if !p.title.is_empty() {
            return Some(p.title.clone());
        }
    }
    None
}

/// Position→new-name diff. Only renames a tab whose current name is a Zellij
/// default OR equals the name we last auto-applied (clobber guard); and only
/// when the desired name differs from the current name (change/loop guard).
pub fn compute_renames(
    tabs: &[(usize, String)],
    tab_panes: &HashMap<usize, Vec<PaneLite>>,
    store: &StateStore,
    applied: &HashMap<usize, String>,
) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (pos, current) in tabs {
        let empty = Vec::new();
        let panes = tab_panes.get(pos).unwrap_or(&empty);
        let Some(desired) = computed_name(panes, store) else {
            continue;
        };
        if &desired == current {
            continue;
        }
        let ours = applied.get(pos).map_or(false, |n| n == current);
        if is_default_name(current) || ours {
            out.push((*pos, desired));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::StatusPayload;
    use crate::state::StateStore;
    use crate::status::Status;
    use std::collections::HashMap;

    fn store_with(id: u32, repo: &str) -> StateStore {
        let mut s = StateStore::default();
        s.apply(
            StatusPayload {
                pane_id: id,
                status: Status::Running,
                repo: repo.into(),
                branch: "b".into(),
                msg: "m".into(),
                on_focus: None,
                seq: None,
                source: "test".into(),
            },
            1,
        );
        s
    }

    #[test]
    fn pane_lite_defaults_are_empty() {
        let p = PaneLite::default();
        assert_eq!(p.id, 0);
        assert!(p.title.is_empty());
        assert!(!p.is_focused);
    }

    #[test]
    fn is_default_name_matches_zellij_default() {
        assert!(is_default_name("Tab #1"));
        assert!(is_default_name("Tab #12"));
        assert!(!is_default_name("Tab #"));
        assert!(!is_default_name("pinky"));
        assert!(!is_default_name("Tab #x"));
    }

    #[test]
    fn computed_name_prefers_agent_repo() {
        let store = store_with(7, "pinky");
        let panes = vec![PaneLite { id: 7, title: "nvim".into(), is_focused: true }];
        assert_eq!(computed_name(&panes, &store), Some("pinky".into()));
    }

    #[test]
    fn computed_name_falls_back_to_focused_title() {
        let store = StateStore::default();
        let panes = vec![
            PaneLite { id: 1, title: "bash".into(), is_focused: false },
            PaneLite { id: 2, title: "nvim".into(), is_focused: true },
        ];
        assert_eq!(computed_name(&panes, &store), Some("nvim".into()));
    }

    #[test]
    fn computed_name_none_when_no_signal() {
        let store = StateStore::default();
        let panes = vec![PaneLite { id: 1, title: "".into(), is_focused: false }];
        assert_eq!(computed_name(&panes, &store), None);
    }

    #[test]
    fn compute_renames_renames_default_skips_manual_and_equal() {
        let store = store_with(7, "pinky");
        let mut tab_panes: HashMap<usize, Vec<PaneLite>> = HashMap::new();
        tab_panes.insert(0, vec![PaneLite { id: 7, title: "x".into(), is_focused: true }]); // default name → rename
        tab_panes.insert(1, vec![PaneLite { id: 7, title: "x".into(), is_focused: true }]); // manual name → skip
        tab_panes.insert(2, vec![PaneLite { id: 7, title: "x".into(), is_focused: true }]); // already == desired → skip
        let tabs = vec![
            (0, "Tab #1".to_string()),
            (1, "my-manual-name".to_string()),
            (2, "pinky".to_string()),
        ];
        let applied = HashMap::new();
        let out = compute_renames(&tabs, &tab_panes, &store, &applied);
        assert_eq!(out, vec![(0, "pinky".to_string())]);
    }

    #[test]
    fn compute_renames_updates_its_own_prior_name() {
        // tab currently shows our last auto-applied name, but the desired name changed.
        let store = store_with(7, "newrepo");
        let mut tab_panes: HashMap<usize, Vec<PaneLite>> = HashMap::new();
        tab_panes.insert(0, vec![PaneLite { id: 7, title: "x".into(), is_focused: true }]);
        let tabs = vec![(0, "oldrepo".to_string())];
        let mut applied = HashMap::new();
        applied.insert(0usize, "oldrepo".to_string()); // we set "oldrepo" before
        let out = compute_renames(&tabs, &tab_panes, &store, &applied);
        assert_eq!(out, vec![(0, "newrepo".to_string())]);
    }
}
