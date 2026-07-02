//! Tab naming policy: the deep module that decides what each tab is called.
//!
//! This is the domain operation named "Tab naming" in `CONTEXT.md`. It owns the
//! naming *policy* (the candidate space repo > worktree > title, focused-first)
//! and the naming *state* (`applied`: which name this module last applied to each
//! tab, the basis for stickiness). It is fed resolved [`TabFacts`] — the caller
//! does the joins across its stores and pane topology, so this module never
//! learns about `StatusStore`, `CommandStore`, `TerminalPane`, or a cwd map.
//!
//! Stickiness in one candidate space: `computed_name` takes the top candidate and
//! `name_supported` asks whether a name sits anywhere in that same space. Both
//! derive from the single `name_candidates` list, so they cannot disagree about
//! what a tab *could* be called — an applied name stays put while any pane still
//! justifies it, and is re-picked only once none does.

use crate::config::NamingMode;
use crate::radar_state::TabId;
use std::collections::HashMap;

/// The naming facts for one pane, resolved by the caller. `repo` is the only
/// fact this module cannot derive itself (it comes from the status store); the
/// raw `cwd` and `title` are processed here (worktree resolution, basename,
/// activity-prefix stripping). `repo` may be `Some("")` — the empty filter lives
/// in [`name_candidates`], matching the pre-extraction behavior.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PaneFacts {
    pub repo: Option<String>,
    pub cwd: Option<String>,
    pub title: String,
    pub focused: bool,
}

/// The naming facts for one tab: its identity, current name, position, and the
/// per-pane facts to draw a name from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabFacts {
    pub id: TabId,
    pub name: String,
    pub position: usize,
    pub panes: Vec<PaneFacts>,
}

/// A requested tab rename — this module's output vocabulary. `radar_state` uses
/// it in `RadarChange`; the runtime turns it into a `RenameTab` effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabRename {
    pub position: usize,
    pub name: String,
}

/// The tab-naming module. Holds the applied-name state behind a single
/// behaviour: [`TabNamer::rename`].
#[derive(Default)]
pub(crate) struct TabNamer {
    /// The name this module last applied to each tab, keyed by stable `TabId`.
    /// The basis for stickiness: a name is "ours" while `applied[id] ==` the
    /// tab's current name.
    applied: HashMap<TabId, String>,
}

impl TabNamer {
    /// Compute the renames to apply across `tabs` under `mode`.
    ///
    /// `Off` renames nothing. Otherwise each tab keeps a name we applied as long
    /// as some pane still justifies it (stickiness); else it re-picks its
    /// preferred name. A re-pick is emitted only when we may overwrite the
    /// current name: `Force` (always), a default `Tab #N`, or a name we applied
    /// (`Managed` never clobbers a manual rename).
    pub(crate) fn rename(&mut self, tabs: &[TabFacts], mode: NamingMode) -> Vec<TabRename> {
        if mode == NamingMode::Off {
            return Vec::new();
        }
        let force = mode == NamingMode::Force;
        let mut out = Vec::new();
        for tab in tabs {
            let ours = self.applied.get(&tab.id) == Some(&tab.name);
            // Stickiness: a name we applied stays put as long as some pane still
            // justifies it, so moving focus between panes in different repos does
            // not flip the tab name. We only re-pick once no pane supports it
            // (e.g. the pane that named the tab closed).
            if ours && name_supported(&tab.panes, &tab.name) {
                continue;
            }
            // Sanitized exactly like the tab intake sanitizes host names: what
            // we apply must equal what `TabUpdate` echoes back, or stickiness
            // would misread our own (re-sanitized) name as a manual rename.
            let Some(desired) = computed_name(&tab.panes).map(|n| crate::payload::sanitize(&n, 40)) else {
                continue;
            };
            if desired.is_empty() || desired == tab.name {
                continue;
            }
            if force || is_default_name(&tab.name) || ours {
                self.applied.insert(tab.id, desired.clone());
                out.push(TabRename {
                    position: tab.position,
                    name: desired,
                });
            }
        }
        out
    }

    /// Forget applied-name state for tabs no longer present. `TabId` is Zellij's
    /// *stable* (non-recycled) tab id, so a stale entry could never mis-apply to
    /// a new tab — this just keeps `applied` from accreting closed tabs over the
    /// life of the (per-tab) plugin instance. Called on every `tabs_changed`,
    /// which always carries the full current tab set.
    pub(crate) fn retain_tabs(&mut self, live: &std::collections::HashSet<TabId>) {
        self.applied.retain(|id, _| live.contains(id));
    }

    #[cfg(test)]
    pub(crate) fn applied_name(&self, id: TabId) -> Option<&str> {
        self.applied.get(&id).map(String::as_str)
    }
}

/// The ordered space of names a tab could take, highest priority first: the
/// focused pane's repo, then any pane's repo, then focused/any worktree-resolved
/// cwd, then focused/any pane title. [`computed_name`] takes the first;
/// [`name_supported`] asks whether a name sits anywhere in this space. Deriving
/// both from this one list is what keeps applied-name stickiness
/// (`name_supported`) in lockstep with what the renamer would actually pick
/// (`computed_name`) — they cannot disagree about the candidate space because
/// there is only one.
fn name_candidates(panes: &[PaneFacts]) -> Vec<String> {
    let repo_of = |p: &PaneFacts| p.repo.clone().filter(|r| !r.is_empty());
    let worktree_of = |p: &PaneFacts| p.cwd.as_deref().and_then(worktree_repo_dir);
    let title_of = |p: &PaneFacts| title_name(&p.title);
    let focused = panes.iter().find(|p| p.focused);

    let mut out = Vec::new();
    out.extend(focused.and_then(&repo_of));
    out.extend(panes.iter().filter_map(&repo_of));
    out.extend(focused.and_then(&worktree_of));
    out.extend(panes.iter().filter_map(&worktree_of));
    out.extend(focused.and_then(&title_of));
    out.extend(panes.iter().filter_map(&title_of));
    out
}

/// The tab's preferred name: the top of [`name_candidates`].
fn computed_name(panes: &[PaneFacts]) -> Option<String> {
    name_candidates(panes).into_iter().next()
}

/// Does any pane still justify `name`? True when `name` is anywhere in
/// [`name_candidates`] — used to keep an applied name "sticky" so focus changes
/// between panes don't churn it.
fn name_supported(panes: &[PaneFacts], name: &str) -> bool {
    name_candidates(panes).iter().any(|c| c == name)
}

fn title_name(title: &str) -> Option<String> {
    let trimmed = title.trim_start();
    let stable = strip_activity_prefix(trimmed).trim();
    if stable.is_empty() {
        None
    } else {
        Some(stable.to_string())
    }
}

fn is_default_name(name: &str) -> bool {
    name.strip_prefix("Tab #")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn cwd_basename(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .rsplit('/')
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

/// Path markers for agent-managed git worktrees created under the standard
/// conventions: Claude (`<repo>/.claude/worktrees/<branch>`) and Codex
/// (`<repo>/.Codex/worktrees/<branch>`). A cwd sitting inside such a worktree
/// belongs, conceptually, to the parent repo — so naming should follow the repo,
/// not the (per-branch) worktree directory. Mirrors the CLI's git-common-dir
/// resolution (`cli::notify`) without needing a `git` process from inside wasm.
const WORKTREE_MARKERS: [&str; 2] = ["/.claude/worktrees/", "/.Codex/worktrees/"];

/// Resolve the directory NAME a tab should take from a pane's cwd. For a cwd
/// under a [`WORKTREE_MARKERS`] segment, this is the parent repo's basename (so
/// every worktree of `zj-radar` reads as `zj-radar` instead of flipping to each
/// branch dir); otherwise it is the plain cwd basename.
fn worktree_repo_dir(path: &str) -> Option<String> {
    match WORKTREE_MARKERS.iter().filter_map(|m| path.find(m)).min() {
        Some(idx) => cwd_basename(&path[..idx]),
        None => cwd_basename(path),
    }
}

fn strip_activity_prefix(title: &str) -> &str {
    let Some(first) = title.chars().next() else {
        return title;
    };
    if !('\u{2800}'..='\u{28ff}').contains(&first) {
        return title;
    }
    let rest = &title[first.len_utf8()..];
    if rest.chars().next().is_some_and(char::is_whitespace) {
        rest
    } else {
        title
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a pane carrying just a repo (unfocused unless `focused`).
    fn repo_pane(repo: &str, focused: bool) -> PaneFacts {
        PaneFacts {
            repo: Some(repo.into()),
            focused,
            ..PaneFacts::default()
        }
    }

    /// Build a single-position tab with the given current name and panes.
    fn tab(id: usize, name: &str, panes: Vec<PaneFacts>) -> TabFacts {
        TabFacts {
            id: TabId::new(id),
            name: name.into(),
            position: 0,
            panes,
        }
    }

    fn renamed_to(name: &str) -> Vec<TabRename> {
        vec![TabRename {
            position: 0,
            name: name.into(),
        }]
    }

    #[test]
    fn cwd_basename_handles_normal_trailing_root_and_empty_paths() {
        assert_eq!(
            cwd_basename("/Users/m/dev/zj-radar"),
            Some("zj-radar".into())
        );
        assert_eq!(
            cwd_basename("/Users/m/dev/zj-radar/"),
            Some("zj-radar".into())
        );
        assert_eq!(cwd_basename("/"), None);
        assert_eq!(cwd_basename(""), None);
    }

    #[test]
    fn default_name_matches_zellij_tab_numbers_only() {
        assert!(is_default_name("Tab #1"));
        assert!(is_default_name("Tab #12"));
        assert!(!is_default_name("Tab #"));
        assert!(!is_default_name("Tab #x"));
        assert!(!is_default_name("custom"));
    }

    #[test]
    fn computed_name_prefers_focused_repo_then_cwd_then_title() {
        // Focused pane's repo wins over an unfocused pane's repo.
        let panes = vec![repo_pane("repo-one", false), repo_pane("repo-two", true)];
        assert_eq!(computed_name(&panes), Some("repo-two".into()));

        // No repo anywhere → focused pane's cwd basename.
        let panes = vec![
            PaneFacts {
                cwd: Some("/work/one".into()),
                title: "one".into(),
                ..PaneFacts::default()
            },
            PaneFacts {
                cwd: Some("/work/two".into()),
                title: "two".into(),
                focused: true,
                ..PaneFacts::default()
            },
        ];
        assert_eq!(computed_name(&panes), Some("two".into()));

        // No repo, no cwd → focused pane's title, with the activity prefix stripped.
        let panes = vec![
            PaneFacts {
                title: "first".into(),
                ..PaneFacts::default()
            },
            PaneFacts {
                title: "⠀ spinner-title".into(),
                focused: true,
                ..PaneFacts::default()
            },
        ];
        assert_eq!(computed_name(&panes), Some("spinner-title".into()));
    }

    #[test]
    fn computed_name_falls_back_to_the_first_pane_that_has_a_title() {
        // No repo, no cwd, nothing focused; the first pane has no usable title
        // but a later one does. The name falls through to that pane's title
        // rather than giving up — the title tier mirrors name_supported, which
        // already accepts any pane's title.
        let panes = vec![
            PaneFacts {
                title: "   ".into(),
                ..PaneFacts::default()
            },
            PaneFacts {
                title: "scratch".into(),
                ..PaneFacts::default()
            },
        ];
        assert_eq!(computed_name(&panes), Some("scratch".into()));
    }

    #[test]
    fn every_computed_name_is_supported() {
        // computed_name and name_supported share one candidate space, so any
        // name computed_name can yield must be "supported" (sticky) — and any
        // pane attribute name_supported accepts must be computable. This pins
        // the two against drift across repo / worktree / title tiers.
        let panes = vec![
            PaneFacts {
                repo: Some("repo-one".into()),
                title: "t1".into(),
                ..PaneFacts::default()
            },
            PaneFacts {
                cwd: Some("/work/two".into()),
                title: "t2".into(),
                focused: true,
                ..PaneFacts::default()
            },
        ];
        let name = computed_name(&panes).expect("a name should be computable here");
        assert!(
            name_supported(&panes, &name),
            "computed name {name:?} must be considered supported"
        );
        // A non-focused, non-first pane's title is both supported AND computable
        // (the case that used to diverge).
        assert!(name_supported(&panes, "t1"));
    }

    #[test]
    fn worktree_repo_dir_resolves_claude_worktree_paths_to_parent_repo() {
        // A worktree under the standard `<repo>/.claude/worktrees/<branch>` path
        // resolves to the PARENT repo's basename, not the branch dir.
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar/.claude/worktrees/feat-x"),
            Some("zj-radar".into())
        );
        // Deeper cwd inside the worktree still resolves to the repo.
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar/.claude/worktrees/feat-x/src/app"),
            Some("zj-radar".into())
        );
        // Codex worktrees live under `<repo>/.Codex/worktrees/<branch>`.
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar/.Codex/worktrees/sidebar-ui-polish"),
            Some("zj-radar".into())
        );
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar/.Codex/worktrees/sidebar-ui-polish/src"),
            Some("zj-radar".into())
        );
        // A normal (non-worktree) path keeps its plain basename.
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar"),
            Some("zj-radar".into())
        );
        assert_eq!(
            worktree_repo_dir("/Users/m/dev/zj-radar/src"),
            Some("src".into())
        );
    }

    #[test]
    fn computed_name_resolves_worktree_cwd_to_parent_repo() {
        let panes = vec![PaneFacts {
            cwd: Some("/Users/m/dev/zj-radar/.claude/worktrees/feat-x".into()),
            focused: true,
            ..PaneFacts::default()
        }];
        assert_eq!(computed_name(&panes), Some("zj-radar".into()));
    }

    #[test]
    fn off_renames_nothing() {
        let mut namer = TabNamer::default();
        let tabs = vec![tab(1, "Tab #1", vec![repo_pane("repo", true)])];
        assert!(namer.rename(&tabs, NamingMode::Off).is_empty());
    }

    #[test]
    fn default_named_tab_gets_named_and_remembered() {
        let mut namer = TabNamer::default();
        let tabs = vec![tab(1, "Tab #1", vec![repo_pane("alpha", true)])];
        assert_eq!(namer.rename(&tabs, NamingMode::Managed), renamed_to("alpha"));
        assert_eq!(namer.applied_name(TabId::new(1)), Some("alpha"));
    }

    #[test]
    fn already_correct_name_is_not_re_emitted() {
        // The tab already carries its preferred name (e.g. the host echoed our
        // rename back): no rename, even though we'd compute the same name.
        let mut namer = TabNamer::default();
        let tabs = vec![tab(1, "alpha", vec![repo_pane("alpha", true)])];
        assert!(namer.rename(&tabs, NamingMode::Managed).is_empty());
    }

    #[test]
    fn applied_names_round_trip_through_the_tab_intake_sanitize() {
        // A candidate over the 40-char intake cap: the name we apply must equal
        // what `TabUpdate` echoes back (post-sanitize), or Managed would
        // misread its own name as a manual rename and re-emit forever.
        let long = "a".repeat(50);
        let mut namer = TabNamer::default();
        let out = namer.rename(&[tab(1, "Tab #1", vec![repo_pane(&long, true)])], NamingMode::Managed);
        let applied = out[0].name.clone();
        assert_eq!(applied.chars().count(), 40, "applied name is pre-capped");
        // The host echoes the sanitized name back: recognized as ours, settled.
        let out = namer.rename(&[tab(1, &applied, vec![repo_pane(&long, true)])], NamingMode::Managed);
        assert!(out.is_empty(), "no rename fight after the echo");
    }

    #[test]
    fn managed_skips_manual_name_but_force_overrides() {
        let mut namer = TabNamer::default();
        let tabs = vec![tab(1, "manual", vec![repo_pane("alpha", true)])];
        // Managed never clobbers a name we did not apply (a manual rename).
        assert!(namer.rename(&tabs, NamingMode::Managed).is_empty());
        assert_eq!(namer.applied_name(TabId::new(1)), None);
        // Force overrides it.
        assert_eq!(namer.rename(&tabs, NamingMode::Force), renamed_to("alpha"));
    }

    #[test]
    fn applied_name_is_sticky_while_any_pane_justifies_it() {
        let mut namer = TabNamer::default();
        // Focused `alpha` names the tab; `beta` also present.
        let tabs = vec![tab(
            1,
            "Tab #1",
            vec![repo_pane("alpha", true), repo_pane("beta", false)],
        )];
        assert_eq!(namer.rename(&tabs, NamingMode::Managed), renamed_to("alpha"));
        // Host echoes the rename; focus shifts to `beta`. `alpha` is still
        // justified by the other pane, so the name must NOT churn.
        let tabs = vec![tab(
            1,
            "alpha",
            vec![repo_pane("alpha", false), repo_pane("beta", true)],
        )];
        assert!(namer.rename(&tabs, NamingMode::Managed).is_empty());
        assert_eq!(namer.applied_name(TabId::new(1)), Some("alpha"));
    }

    #[test]
    fn retain_tabs_forgets_closed_tabs_and_keeps_live_ones() {
        let mut namer = TabNamer::default();
        let tabs = vec![
            tab(1, "Tab #1", vec![repo_pane("alpha", true)]),
            tab(2, "Tab #2", vec![repo_pane("beta", true)]),
        ];
        namer.rename(&tabs, NamingMode::Managed);
        assert_eq!(namer.applied_name(TabId::new(1)), Some("alpha"));
        assert_eq!(namer.applied_name(TabId::new(2)), Some("beta"));

        // Tab 1 closes; only tab 2 remains in the live set.
        let live = std::collections::HashSet::from([TabId::new(2)]);
        namer.retain_tabs(&live);
        assert_eq!(
            namer.applied_name(TabId::new(1)),
            None,
            "a closed tab's applied name is dropped"
        );
        assert_eq!(
            namer.applied_name(TabId::new(2)),
            Some("beta"),
            "a live tab's applied name is kept"
        );
    }

    #[test]
    fn repicks_when_the_applied_name_loses_all_support() {
        let mut namer = TabNamer::default();
        let tabs = vec![tab(
            1,
            "Tab #1",
            vec![repo_pane("alpha", true), repo_pane("beta", false)],
        )];
        assert_eq!(namer.rename(&tabs, NamingMode::Managed), renamed_to("alpha"));
        // Host echoes; the `alpha` pane closes, leaving only `beta`. `alpha` is no
        // longer supported, so the tab re-picks from the survivor.
        let tabs = vec![tab(1, "alpha", vec![repo_pane("beta", true)])];
        assert_eq!(namer.rename(&tabs, NamingMode::Managed), renamed_to("beta"));
        assert_eq!(namer.applied_name(TabId::new(1)), Some("beta"));
    }
}
