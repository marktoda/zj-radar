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
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// Returns the last non-empty path component (basename) of `path`, or `None`
/// if the path is empty, root-only, or has no meaningful basename.
///
/// Examples:
/// - `/Users/m/dev/zj-radar` → `Some("zj-radar")`
/// - `/Users/m/dev/zj-radar/` → `Some("zj-radar")` (trailing slash trimmed)
/// - `/` → `None`
/// - `` → `None`
pub fn cwd_basename(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .rsplit('/')
        .find(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Desired display name for one tab, or None if no push signal is available.
/// Precedence:
///   (a) agent **repo** from `store` (focused pane first, then any)
///   (b) **cwd_basename** from `pane_cwd` (focused pane first, then any)
///   (c) pane **title** (focused first, then first pane)
pub fn computed_name(
    panes: &[PaneLite],
    store: &StateStore,
    pane_cwd: &HashMap<u32, String>,
) -> Option<String> {
    let repo_of = |p: &PaneLite| {
        store
            .get(p.id)
            .map(|s| s.repo.clone())
            .filter(|r| !r.is_empty())
    };
    let focused = panes.iter().find(|p| p.is_focused);
    // (a) agent repo — focused pane first
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
    // (b) cwd basename — focused pane first
    if let Some(p) = focused {
        if let Some(cwd) = pane_cwd.get(&p.id) {
            if let Some(name) = cwd_basename(cwd) {
                return Some(name);
            }
        }
    }
    for p in panes {
        if let Some(cwd) = pane_cwd.get(&p.id) {
            if let Some(name) = cwd_basename(cwd) {
                return Some(name);
            }
        }
    }
    // (c) pane title
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

/// Position→new-name diff. By default only renames a tab whose current name is
/// a Zellij default OR equals the name we last auto-applied (clobber guard), so
/// user-chosen names are never stomped. With `force`, that guard is bypassed and
/// any tab with a name signal is renamed to it. The change/loop guard (skip when
/// desired == current) always applies, so force can't cause a rename loop.
pub fn compute_renames(
    tabs: &[(usize, String)],
    tab_panes: &HashMap<usize, Vec<PaneLite>>,
    store: &StateStore,
    applied: &HashMap<usize, String>,
    force: bool,
    pane_cwd: &HashMap<u32, String>,
) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (pos, current) in tabs {
        let empty = Vec::new();
        let panes = tab_panes.get(pos).unwrap_or(&empty);
        let Some(desired) = computed_name(panes, store, pane_cwd) else {
            continue;
        };
        if &desired == current {
            continue;
        }
        let ours = applied.get(pos) == Some(current);
        if force || is_default_name(current) || ours {
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

    fn no_cwd() -> HashMap<u32, String> {
        HashMap::new()
    }

    // ── cwd_basename tests ──

    #[test]
    fn cwd_basename_normal_path() {
        assert_eq!(cwd_basename("/Users/m/dev/zj-radar"), Some("zj-radar".into()));
    }

    #[test]
    fn cwd_basename_trailing_slash() {
        assert_eq!(cwd_basename("/Users/m/dev/zj-radar/"), Some("zj-radar".into()));
    }

    #[test]
    fn cwd_basename_root() {
        assert_eq!(cwd_basename("/"), None);
    }

    #[test]
    fn cwd_basename_empty() {
        assert_eq!(cwd_basename(""), None);
    }

    // ── PaneLite defaults ──

    #[test]
    fn pane_lite_defaults_are_empty() {
        let p = PaneLite::default();
        assert_eq!(p.id, 0);
        assert!(p.title.is_empty());
        assert!(!p.is_focused);
    }

    // ── is_default_name ──

    #[test]
    fn is_default_name_matches_zellij_default() {
        assert!(is_default_name("Tab #1"));
        assert!(is_default_name("Tab #12"));
        assert!(!is_default_name("Tab #"));
        assert!(!is_default_name("pinky"));
        assert!(!is_default_name("Tab #x"));
    }

    // ── computed_name tests ──

    #[test]
    fn computed_name_prefers_agent_repo() {
        let store = store_with(7, "pinky");
        let panes = vec![PaneLite { id: 7, title: "nvim".into(), is_focused: true }];
        assert_eq!(computed_name(&panes, &store, &no_cwd()), Some("pinky".into()));
    }

    #[test]
    fn computed_name_falls_back_to_focused_title() {
        let store = StateStore::default();
        let panes = vec![
            PaneLite { id: 1, title: "bash".into(), is_focused: false },
            PaneLite { id: 2, title: "nvim".into(), is_focused: true },
        ];
        assert_eq!(computed_name(&panes, &store, &no_cwd()), Some("nvim".into()));
    }

    #[test]
    fn computed_name_none_when_no_signal() {
        let store = StateStore::default();
        let panes = vec![PaneLite { id: 1, title: "".into(), is_focused: false }];
        assert_eq!(computed_name(&panes, &store, &no_cwd()), None);
    }

    #[test]
    fn computed_name_cwd_beats_title() {
        let store = StateStore::default();
        let panes = vec![PaneLite { id: 1, title: "bash".into(), is_focused: true }];
        let mut cwd = HashMap::new();
        cwd.insert(1u32, "/Users/m/dev/myproject".to_string());
        // cwd basename "myproject" should win over title "bash"
        assert_eq!(computed_name(&panes, &store, &cwd), Some("myproject".into()));
    }

    #[test]
    fn computed_name_agent_repo_beats_cwd() {
        let store = store_with(7, "pinky");
        let panes = vec![PaneLite { id: 7, title: "nvim".into(), is_focused: true }];
        let mut cwd = HashMap::new();
        cwd.insert(7u32, "/Users/m/dev/some-other-dir".to_string());
        // agent repo "pinky" should win over cwd
        assert_eq!(computed_name(&panes, &store, &cwd), Some("pinky".into()));
    }

    #[test]
    fn computed_name_focused_cwd_wins_over_nonfocused() {
        let store = StateStore::default();
        let panes = vec![
            PaneLite { id: 1, title: "".into(), is_focused: false },
            PaneLite { id: 2, title: "".into(), is_focused: true },
        ];
        let mut cwd = HashMap::new();
        cwd.insert(1u32, "/Users/m/dev/non-focused-dir".to_string());
        cwd.insert(2u32, "/Users/m/dev/focused-dir".to_string());
        // focused pane's cwd should win
        assert_eq!(computed_name(&panes, &store, &cwd), Some("focused-dir".into()));
    }

    // ── compute_renames tests ──

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
        let out = compute_renames(&tabs, &tab_panes, &store, &applied, false, &no_cwd());
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
        let out = compute_renames(&tabs, &tab_panes, &store, &applied, false, &no_cwd());
        assert_eq!(out, vec![(0, "newrepo".to_string())]);
    }

    #[test]
    fn compute_renames_force_overrides_manual_name() {
        // force=true bypasses the clobber guard: a user-chosen name is renamed.
        let store = store_with(7, "pinky");
        let mut tab_panes: HashMap<usize, Vec<PaneLite>> = HashMap::new();
        tab_panes.insert(0, vec![PaneLite { id: 7, title: "x".into(), is_focused: true }]);
        let tabs = vec![(0, "my-manual-name".to_string())];
        let applied = HashMap::new();
        // default behavior leaves the manual name alone...
        assert!(compute_renames(&tabs, &tab_panes, &store, &applied, false, &no_cwd()).is_empty());
        // ...but force renames it to the agent repo.
        assert_eq!(
            compute_renames(&tabs, &tab_panes, &store, &applied, true, &no_cwd()),
            vec![(0, "pinky".to_string())]
        );
    }

    #[test]
    fn compute_renames_force_still_skips_when_equal() {
        // change/loop guard holds even under force (desired == current → no-op).
        let store = store_with(7, "pinky");
        let mut tab_panes: HashMap<usize, Vec<PaneLite>> = HashMap::new();
        tab_panes.insert(0, vec![PaneLite { id: 7, title: "x".into(), is_focused: true }]);
        let tabs = vec![(0, "pinky".to_string())];
        let applied = HashMap::new();
        assert!(compute_renames(&tabs, &tab_panes, &store, &applied, true, &no_cwd()).is_empty());
    }

    #[test]
    fn compute_renames_uses_cwd_for_default_named_tab() {
        // A plain tab named "Tab #1" with no agent state but a known cwd
        // should be renamed to the cwd's basename.
        let store = StateStore::default();
        let mut tab_panes: HashMap<usize, Vec<PaneLite>> = HashMap::new();
        tab_panes.insert(0, vec![PaneLite { id: 5, title: "".into(), is_focused: true }]);
        let tabs = vec![(0, "Tab #1".to_string())];
        let applied = HashMap::new();
        let mut cwd = HashMap::new();
        cwd.insert(5u32, "/Users/m/dev/myproject".to_string());
        let out = compute_renames(&tabs, &tab_panes, &store, &applied, false, &cwd);
        assert_eq!(out, vec![(0, "myproject".to_string())]);
    }
}
