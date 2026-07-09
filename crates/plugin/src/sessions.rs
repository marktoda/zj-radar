//! Cross-session peer state + the Alt+[/] cycle state machine — pure: no
//! zellij-tile, no filesystem. The runtime (Task 5) feeds this module parsed
//! facts (the live session list, peer presence JSON, this session's own
//! counts) and it derives the ordered badge on demand, mirroring the
//! rows-derived-on-render doctrine (`CONTEXT.md`) rather than caching a
//! badge that could drift from `live`/`peers`/`own`.
//!
//! Ordering is a single source of truth shared by `badge()` (what's shown)
//! and `cycle()` (what Alt+[/] steps through): current session first, then
//! `attention > 0` sessions by name, then the rest by name.

use crate::presence::Presence;
use crate::radar_state::Direction;
use std::collections::HashSet;

/// A live session as reported by Zellij's session list, reduced to what this
/// module needs. `lib.rs` maps the host's `SessionInfo` into this.
pub(crate) struct SessionLite {
    pub name: String,
    pub is_current: bool,
}

/// One row of the cross-session badge, in display order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BadgeEntry {
    pub name: String,
    pub running: usize,
    pub attention: usize,
    pub attention_tab_position: Option<usize>,
    pub is_current: bool,
    pub selected: bool,
}

/// Where a committed cycle gesture lands — enough for the runtime to switch
/// sessions and then jump straight to the tab that needs attention there.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CommitTarget {
    pub name: String,
    pub attention_tab_position: Option<usize>,
}

/// A pending (not yet committed) cycle selection. `name` identifies the
/// selected session directly rather than its position in the derived order —
/// `update_live`/`update_presences` can reorder that list between the
/// `cycle()` tap and the commit `tick()` (e.g. a new session sorting ahead of
/// the selected one), and a positional index would then silently retarget
/// whatever session ended up at the old index. Re-resolved by name against
/// the fresh order on every `cycle()`/`tick()` call instead. `last_tap_tick`
/// is the tick of the most recent `cycle()` call, so `tick()` can tell a
/// same-tick race (the tap and the idle timer landing in the same update)
/// from genuine idle — only the latter may commit.
struct SelectionState {
    name: String,
    last_tap_tick: u64,
}

#[derive(Default)]
pub(crate) struct Sessions {
    live: Vec<SessionLite>,
    peers: Vec<Presence>,
    own: Option<Presence>,
    selection: Option<SelectionState>,
}

impl Sessions {
    /// Replace the live session list (from Zellij's session list). Returns
    /// whether the derived badge actually changed, so the runtime only
    /// repaints on real content change.
    pub(crate) fn update_live(&mut self, live: Vec<SessionLite>) -> bool {
        let before = self.badge();
        self.live = live;
        self.badge() != before
    }

    /// Parse peer presence JSON (one string per peer session) and keep only
    /// the ones naming a currently-live session — a corrupt file (handled by
    /// `Presence::parse`'s leniency) or a session that has since closed must
    /// never leave a stale/bogus badge entry lying around.
    pub(crate) fn update_presences(&mut self, raw: Vec<String>) -> bool {
        let before = self.badge();
        let live_names: HashSet<&str> = self.live.iter().map(|s| s.name.as_str()).collect();
        self.peers = raw
            .iter()
            .filter_map(|s| Presence::parse(s))
            .filter(|p| live_names.contains(p.session_name.as_str()))
            .collect();
        self.badge() != before
    }

    /// Record this session's own counts (never read from a peer file — the
    /// current session knows its own state directly).
    pub(crate) fn set_own(&mut self, p: Presence) -> bool {
        let before = self.badge();
        self.own = Some(p);
        self.badge() != before
    }

    /// Advance (or start) the cycle selection and stamp the tick it happened
    /// on. Re-derives the shared ordering fresh each call, so a selection
    /// started before a `update_live`/`update_presences` change always steps
    /// relative to the current membership, never a stale snapshot. The
    /// pending selection's *name* is re-resolved against that fresh order
    /// (not trusted as a stale index) before advancing from it.
    pub(crate) fn cycle(&mut self, dir: Direction, now_tick: u64) -> bool {
        let before = self.badge();
        let order = self.ordered();
        // A previously selected name that no longer appears in the fresh
        // order (its session closed) is treated the same as "nothing
        // selected" — restart below rather than advance from a position it
        // no longer occupies.
        let current_position =
            self.selection.as_ref().and_then(|sel| order.iter().position(|s| s.name == sel.name));
        self.selection = match (current_position, order.len()) {
            (_, 0) => None,
            (None, _) => {
                // Nothing selected yet (or the selection vanished): start at
                // the first non-current entry (index 0 is the current
                // session, when live includes one).
                order
                    .iter()
                    .position(|s| !s.is_current)
                    .map(|index| SelectionState { name: order[index].name.clone(), last_tap_tick: now_tick })
            }
            (Some(index), len) => {
                let next = match dir {
                    Direction::Next => (index + 1) % len,
                    Direction::Prev => (index + len - 1) % len,
                };
                Some(SelectionState { name: order[next].name.clone(), last_tap_tick: now_tick })
            }
        };
        self.badge() != before
    }

    /// Idle-commit: fires when a selection is pending and this tick is not
    /// the same tick as the tap that created/advanced it. The selected
    /// *name* is re-resolved against a freshly derived order rather than a
    /// remembered index, so a reorder between the tap and this tick can
    /// never retarget a different session; if the name is no longer live,
    /// the selection is dropped and nothing commits. Landing on the current
    /// session is the cancel gesture (`None`, no effect); either way a
    /// commit clears the pending selection.
    pub(crate) fn tick(&mut self, now_tick: u64) -> Option<CommitTarget> {
        let sel = self.selection.as_ref()?;
        if now_tick <= sel.last_tap_tick {
            return None; // tap and timer racing in the same tick must not commit
        }
        let name = sel.name.clone();
        self.selection = None; // committing, cancelling, or dropping a vanished selection all clear
        let order = self.ordered();
        let entry = order.iter().find(|s| s.name == name)?; // vanished session: nothing to commit
        if entry.is_current {
            return None; // cancel gesture: landing back on the current session
        }
        Some(CommitTarget {
            name: entry.name.clone(),
            attention_tab_position: self.presence_for(&entry.name).and_then(|p| p.attention_tab_position),
        })
    }

    /// The ordered badge: current session first, then attention>0 sessions by
    /// name, then the rest by name. Derived fresh from `live`/`peers`/`own`
    /// every call — never cached — so it can never drift from its inputs.
    pub(crate) fn badge(&self) -> Vec<BadgeEntry> {
        let order = self.ordered();
        let selected_name = self.selection.as_ref().map(|sel| sel.name.as_str());
        order
            .into_iter()
            .map(|s| {
                let p = self.presence_for(&s.name);
                BadgeEntry {
                    name: s.name.clone(),
                    running: p.map_or(0, |p| p.running),
                    attention: p.map_or(0, |p| p.attention),
                    attention_tab_position: p.and_then(|p| p.attention_tab_position),
                    is_current: s.is_current,
                    selected: selected_name == Some(s.name.as_str()),
                }
            })
            .collect()
    }

    /// Whether a cycle selection is pending — the runtime keeps the Fast
    /// timer cadence armed while true, so the idle-commit in `tick()` fires
    /// promptly rather than waiting for the next Slow tick. Only the wasm
    /// runtime (Task 5) reads this in production; pinned by a host test
    /// (`wants_fast_cadence_tracks_pending_selection_through_commit_and_cancel`).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn wants_fast_cadence(&self) -> bool {
        self.selection.is_some()
    }

    /// The shared ordering: current session(s) first, then attention>0
    /// sessions by name, then the rest by name. Both `badge()` and `cycle()`
    /// go through this so "what's shown" and "what Alt+[/] steps through"
    /// can never disagree.
    fn ordered(&self) -> Vec<&SessionLite> {
        let mut current: Vec<&SessionLite> = Vec::new();
        let mut attention: Vec<&SessionLite> = Vec::new();
        let mut rest: Vec<&SessionLite> = Vec::new();
        for s in &self.live {
            if s.is_current {
                current.push(s);
            } else if self.presence_for(&s.name).map_or(0, |p| p.attention) > 0 {
                attention.push(s);
            } else {
                rest.push(s);
            }
        }
        attention.sort_by(|a, b| a.name.cmp(&b.name));
        rest.sort_by(|a, b| a.name.cmp(&b.name));
        current.into_iter().chain(attention).chain(rest).collect()
    }

    /// This session's own counts if `name` is the current session, else the
    /// matching peer's presence, else `None` (no data reported yet).
    fn presence_for(&self, name: &str) -> Option<&Presence> {
        self.own
            .as_ref()
            .filter(|p| p.session_name == name)
            .or_else(|| self.peers.iter().find(|p| p.session_name == name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radar_state::Direction;

    fn live(names: &[(&str, bool)]) -> Vec<SessionLite> {
        names.iter().map(|(n, c)| SessionLite { name: n.to_string(), is_current: *c }).collect()
    }
    fn presence(name: &str, running: usize, attention: usize) -> String {
        format!(r#"{{"session_name":"{name}","running":{running},"attention":{attention}}}"#)
    }

    #[test]
    fn badge_orders_current_then_attention_then_rest() {
        let mut s = Sessions::default();
        s.update_live(live(&[("zeta", false), ("work", true), ("alpha", false)]));
        s.update_presences(vec![presence("zeta", 1, 2), presence("alpha", 1, 0)]);
        s.set_own(Presence { session_name: "work".into(), running: 3, attention: 0,
                             attention_tab_position: None, updated_epoch_s: 0 });
        // (Bound rather than chained straight off `s.badge()`: the literal
        // brief snippet borrows from a temporary `Vec<BadgeEntry>` that would
        // be dropped at the end of the `let` statement — a compile-mechanics
        // issue, not a semantic change; the assertion is unchanged.)
        let badge = s.badge();
        let names: Vec<&str> = badge.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["work", "zeta", "alpha"]);
    }

    #[test]
    fn presences_for_dead_sessions_are_dropped() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true)]));
        s.update_presences(vec![presence("ghost", 5, 5)]);
        assert!(s.badge().iter().all(|b| b.name != "ghost"));
    }

    #[test]
    fn cycle_advances_and_wraps_and_tick_commits_after_idle() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true), ("beta", false), ("alpha", false)]));
        s.update_presences(vec![presence("alpha", 0, 1), presence("beta", 1, 0)]);
        // Order: work(current), alpha(attention), beta.
        s.cycle(Direction::Next, 10);                 // select alpha
        assert_eq!(s.tick(10), None, "same-tick tap must not commit");
        s.cycle(Direction::Next, 11);                 // skip to beta
        let t = s.tick(12).expect("idle tick commits");
        assert_eq!(t.name, "beta");
        assert_eq!(s.tick(13), None, "committed selection is cleared");
    }

    #[test]
    fn committing_on_current_session_is_a_noop_cancel() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true), ("alpha", false)]));
        s.update_presences(vec![presence("alpha", 0, 0)]);
        s.cycle(Direction::Next, 1);                  // alpha
        s.cycle(Direction::Next, 2);                  // wraps to work (current)
        assert_eq!(s.tick(3), None, "landing on the current session cancels");
    }

    #[test]
    fn commit_carries_attention_tab_position() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true), ("alpha", false)]));
        s.update_presences(vec![
            r#"{"session_name":"alpha","running":0,"attention":1,"attention_tab_position":2}"#.to_string(),
        ]);
        s.cycle(Direction::Next, 1);
        assert_eq!(s.tick(2).unwrap().attention_tab_position, Some(2));
    }

    // -- Pinning: wants_fast_cadence() --------------------------------------
    // No wasm caller exercises this from a host test, so a refactor that
    // breaks the "armed while a selection is pending" contract would
    // otherwise go unnoticed until manual testing in Zellij.

    #[test]
    fn wants_fast_cadence_tracks_pending_selection_through_commit_and_cancel() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true), ("alpha", false), ("beta", false)]));
        assert!(!s.wants_fast_cadence(), "nothing selected initially");

        s.cycle(Direction::Next, 1); // selects alpha
        assert!(s.wants_fast_cadence(), "a cycle tap arms a pending selection");

        s.tick(2); // idle commit to alpha
        assert!(!s.wants_fast_cadence(), "a commit clears the pending selection");

        s.cycle(Direction::Next, 3); // alpha
        s.cycle(Direction::Next, 4); // beta
        s.cycle(Direction::Next, 5); // wraps to work (current)
        assert!(s.wants_fast_cadence(), "still pending until the idle tick fires");

        s.tick(6); // cancel-commit (lands on current)
        assert!(!s.wants_fast_cadence(), "a cancel-commit also clears the pending selection");
    }

    // -- Pinning: content-compare returns ------------------------------------
    // Each mutator must report `true` only when the derived badge actually
    // differs, not merely because it was called. A refactor that starts
    // returning `true` unconditionally (or `false` unconditionally) would
    // make the runtime over- or under-repaint without any other test
    // noticing, since none of the tests above assert on the return value.

    #[test]
    fn update_live_reports_change_only_on_actual_content_change() {
        let mut s = Sessions::default();
        assert!(s.update_live(live(&[("work", true)])), "first population changes the badge");
        assert!(!s.update_live(live(&[("work", true)])), "identical live list is not a change");
    }

    #[test]
    fn update_presences_reports_change_only_on_actual_content_change() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true), ("alpha", false)]));
        assert!(s.update_presences(vec![presence("alpha", 1, 0)]), "first presence report changes the badge");
        assert!(!s.update_presences(vec![presence("alpha", 1, 0)]), "identical presence report is not a change");
    }

    #[test]
    fn set_own_reports_change_only_on_actual_content_change() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true)]));
        let p = Presence { session_name: "work".into(), running: 3, attention: 0,
                           attention_tab_position: None, updated_epoch_s: 0 };
        assert!(s.set_own(p.clone()), "first own-count report changes the badge");
        // Same badge-relevant fields, different updated_epoch_s (not part of
        // BadgeEntry) — must not register as a change.
        let p2 = Presence { session_name: "work".into(), running: 3, attention: 0,
                            attention_tab_position: None, updated_epoch_s: 99 };
        assert!(!s.set_own(p2), "a report identical in badge-relevant fields is not a change");
    }

    #[test]
    fn cycle_reports_change_when_the_highlight_moves() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true), ("alpha", false), ("beta", false)]));
        assert!(s.cycle(Direction::Next, 1), "starting a selection flips a `selected` flag in the badge");
        assert!(s.cycle(Direction::Next, 2), "advancing the selection moves the `selected` flag");
    }

    // -- Selection identity (not positional index) ---------------------------
    // `SelectionState` must track the selected session by name, not by its
    // position in the derived order — a reorder between the cycle() tap and
    // the commit tick() must never silently retarget a different session.

    #[test]
    fn cycle_selection_survives_reordering_by_name_not_index() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true), ("alpha", false), ("beta", false)]));
        s.update_presences(vec![presence("alpha", 1, 0), presence("beta", 1, 0)]);
        // Order: work (current), alpha, beta — both zero-attention, by name.
        s.cycle(Direction::Next, 1); // selects alpha (first non-current)

        // "aardvark" joins and sorts ahead of "alpha" in the rest bucket. A
        // positional-index selection (old index 1) would now point at
        // "aardvark" instead of "alpha".
        s.update_live(live(&[("work", true), ("aardvark", false), ("alpha", false), ("beta", false)]));

        let t = s.tick(2).expect("idle tick commits");
        assert_eq!(t.name, "alpha", "selection must follow the named session, not the reordered index");
    }

    #[test]
    fn selected_session_vanishing_before_tick_clears_selection_and_commits_nothing() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true), ("alpha", false)]));
        s.cycle(Direction::Next, 1); // selects alpha
        s.update_live(live(&[("work", true)])); // alpha leaves before the idle tick

        assert_eq!(s.tick(2), None, "a vanished selection must not commit");
        assert!(!s.wants_fast_cadence(), "the cleared selection must not keep fast cadence armed");
    }

    // -- Pinning: cycle restart after vanish and no-op change report -----------
    // Cycle selection must restart fresh when the previously-selected session
    // disappears and a new cycle() call advances the selection. A no-op cycle
    // (nothing to select) must not set up a pending selection or report a
    // badge change.

    #[test]
    fn cycle_restarts_fresh_after_selected_session_vanishes() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true), ("alpha", false), ("beta", false), ("gamma", false)]));
        // No attention: order is work (current), then alpha, beta, gamma by name.

        s.cycle(Direction::Next, 1); // selects alpha (first non-current)

        // alpha vanishes, leaving a fresh order with *two* non-current entries
        // (beta, gamma) — order: work, beta, gamma.
        s.update_live(live(&[("work", true), ("beta", false), ("gamma", false)]));

        // Cycling with Prev is what actually discriminates restart-fresh from
        // an index-0 fallback: restart-fresh ignores the stale direction and
        // always selects the first non-current entry (beta). A regression
        // that resolved the vanished name to index 0 ("as if `work`/current
        // were selected") would instead advance *backward* from there and
        // land on the last entry (gamma) — the same bug that, under
        // Direction::Next, would coincidentally land on beta too and pass
        // silently.
        s.cycle(Direction::Prev, 2);
        let badge = s.badge();
        let selected: Vec<&str> = badge.iter().filter(|b| b.selected).map(|b| b.name.as_str()).collect();
        assert_eq!(
            selected,
            vec!["beta"],
            "restart-fresh must ignore the stale direction and pick the first \
             non-current entry, not fall back to advancing from index 0"
        );

        let t = s.tick(3).expect("idle tick commits");
        assert_eq!(t.name, "beta", "after vanished selection, cycle must restart and select the first non-current entry");
    }

    #[test]
    fn cycle_returns_false_on_noop_with_only_current_session() {
        let mut s = Sessions::default();
        s.update_live(live(&[("work", true)]));
        // No presences, no peers — work is the only session.

        let changed = s.cycle(Direction::Next, 1);
        assert!(!changed, "cycle with nothing to select must return false (no badge change)");
        assert!(!s.wants_fast_cadence(), "cycle with nothing to select must not set up a pending selection");
    }
}
