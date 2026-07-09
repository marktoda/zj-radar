//! Cross-session peer state + the Alt+[/] cycle state machine — pure: no
//! zellij-tile, no filesystem. The runtime feeds this module parsed facts
//! (peer presence JSON + its file's mtime age, this session's own counts)
//! and it derives the ordered badge on demand, mirroring the
//! rows-derived-on-render doctrine (`CONTEXT.md`) rather than caching a
//! badge that could drift from `peers`/`own`.
//!
//! task-14 (user decision): a remembered session must NEVER silently vanish
//! from the badge. `session_files::read_peer_presences` no longer filters
//! anything by mtime — every peer file it finds comes back, forever (until
//! the 6h open-time sweep, the only true forgetting). Liveness is instead a
//! per-entry *staleness* state this module derives from the mtime age it's
//! handed: `fresh` while recently heartbeated, `stale` past
//! [`STALE_AFTER_SECS`] — dimmed on the badge and unreachable via `cycle()`,
//! but never dropped. This module's other leniency is limited to "don't
//! choke on a malformed line" (`Presence::parse`), not "cross-check
//! membership".
//!
//! Ordering is a single source of truth shared by `badge()` (what's shown)
//! and `cycle()` (what Alt+[/] steps through): current session first, then
//! fresh `attention > 0` sessions by name, then the rest of the fresh
//! sessions by name, then every stale session by name — stale entries never
//! jump the queue on attention, since a likely-dead session's last-known
//! attention count isn't actionable.

use std::collections::HashMap;

use crate::presence::Presence;
use crate::radar_state::Direction;

/// How long a peer's presence file may sit unrefreshed before its badge row
/// dims to stale. `runtime.rs`'s timer heartbeats an idle-but-alive
/// session's own file at least once per Slow (60s) tick, so 90s gives 50%
/// margin against a single missed beat before flagging it — generous
/// enough that ordinary scheduler jitter never flickers an entry, but a
/// session that's genuinely gone quiet reads as such promptly. A missed
/// beat marks stale, never vanishes (see the module doc).
pub(crate) const STALE_AFTER_SECS: u64 = 90;

/// A peer's [`Presence`] plus the staleness derived from its presence
/// file's mtime age at the last read (see the module doc). `stale` is a
/// snapshot from that read, not re-evaluated against a live clock — like
/// every other peer fact here, it's only ever as fresh as the last
/// `update_presences` call.
struct Peer {
    presence: Presence,
    stale: bool,
}

/// A row in the shared ordering: a [`Presence`] (own or peer) plus whether
/// it's the current session and whether it's stale. `own` contributes at
/// most one of these (via `set_own`, always fresh — this session knows its
/// own liveness directly); every peer in `peers` contributes one. Combines
/// what used to be a separate `SessionLite` (identity) + a `presence_for`
/// lookup (counts) into a single reference, now that a `Presence` IS the
/// identity (its `session_name`) as well as the counts.
struct OrderedEntry<'a> {
    presence: &'a Presence,
    is_current: bool,
    stale: bool,
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
    pub stale: bool,
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
/// the fresh order on every `cycle()`/`tick()` call instead.
///
/// `taps_since_last_fire` replaces an earlier tick-counter comparison
/// (`now_tick > last_tap_tick`) that had two field-observed bugs: (a) a tap
/// landing just before an already-scheduled fire compared as "idle" and
/// committed instantly, and (b) taps arriving faster than the 1s tick grain
/// all shared one tick number, so the deadline never extended past the
/// FIRST tap in a burst. `cycle()` sets this flag on every tap; `tick()`
/// clears it and skips (no commit) whenever it's set, so a fire only ever
/// commits when its entire covered interval was tap-free — guaranteeing at
/// least one full quiet interval after the LAST tap, at the cost of at most
/// one extra (skipped) fire.
struct SelectionState {
    name: String,
    taps_since_last_fire: bool,
}

#[derive(Default)]
pub(crate) struct Sessions {
    peers: Vec<Peer>,
    own: Option<Presence>,
    selection: Option<SelectionState>,
}

impl Sessions {
    /// Replace the peer set with a fresh read of every OTHER session's
    /// presence file, each paired with its file's mtime age in seconds
    /// (`session_files::read_peer_presences`'s `age_secs` — no longer
    /// filtered by liveness there; see the module doc). The only filtering
    /// left here is `Presence::parse`'s leniency (skip a malformed line,
    /// don't crash on it). Returns whether the derived badge actually
    /// changed, so the runtime only repaints on real content change.
    pub(crate) fn update_presences(&mut self, raw: Vec<(String, u64)>) -> bool {
        let before = self.badge();
        // WHY: presence files are keyed by server pid on disk, not by
        // session name. A session killed and recreated under the same name
        // gets a fresh presence file at a new pid path, but the old one (a
        // "corpse") isn't unlinked — it just sits there parsing as a second
        // peer under the same name until the (now much longer) open-time
        // sweep reaps it. Left undeduped, `badge()` would render that name
        // twice. Collapse by `session_name` here, keeping the presence with
        // the greatest `updated_epoch_s` — that's the newest write, i.e. the
        // live session; the corpse is whatever was on disk before the kill.
        // On a tie, `>` (not `>=`) below makes the later entry in `raw` win
        // deterministically, without needing separate tie-break code. The
        // `stale` flag carried alongside is whichever entry's own mtime age
        // won this dedup — a corpse losing to a fresh recreation of the same
        // name also loses its (typically old) staleness along with it.
        let mut by_name: HashMap<String, Peer> = HashMap::new();
        for (json, age_secs) in raw {
            let Some(p) = Presence::parse(&json) else { continue };
            match by_name.get(&p.session_name) {
                Some(existing) if existing.presence.updated_epoch_s > p.updated_epoch_s => {} // existing is strictly fresher: keep it
                _ => {
                    let stale = age_secs > STALE_AFTER_SECS;
                    by_name.insert(p.session_name.clone(), Peer { presence: p, stale });
                }
            }
        }
        self.peers = by_name.into_values().collect();
        self.badge() != before
    }

    /// Record this session's own counts (never read from a peer file — the
    /// current session knows its own state directly). The single path for
    /// own counts into the badge — the runtime calls this every time it
    /// recomputes `own_presence()`, not just on a name change, so the own
    /// row stays live as running/attention move.
    pub(crate) fn set_own(&mut self, p: Presence) -> bool {
        let before = self.badge();
        self.own = Some(p);
        self.badge() != before
    }

    /// Manually forget a peer by name — the in-memory half of the right-click
    /// dismiss gesture (`PluginRuntime::mouse_right_click`), the user-driven
    /// complement to the 6h open-time sweep for a session the user already
    /// knows is dead. Removing the entry here gives instant visual feedback
    /// (the badge row vanishes on this instance without waiting for the next
    /// file read); the on-disk half is `Effect::DismissPresence`. Only
    /// shrinks `peers` — ordering, dedup, and cycle semantics of the
    /// survivors are untouched, and `own` is never touched (the caller gates
    /// on staleness, and the own entry is never stale). Safe against a
    /// misjudged dismiss by construction: if the session is secretly still
    /// alive, its next heartbeat/edge re-publishes a presence file and the
    /// next `update_presences` simply brings it back, fresh — dismiss is
    /// never destructive to a live session
    /// (`dismissed_but_alive_session_reappears_fresh_on_next_presence_write`).
    /// Returns whether the derived badge actually changed, same contract as
    /// the other mutators here.
    pub(crate) fn dismiss(&mut self, name: &str) -> bool {
        let before = self.badge();
        self.peers.retain(|p| p.presence.session_name != name);
        self.badge() != before
    }

    /// Advance (or start) the cycle selection and arm its tap-since-fire
    /// flag (see `SelectionState`'s doc). Re-derives the shared ordering
    /// fresh each call, so a selection started before a
    /// `update_live`/`update_presences` change always steps relative to the
    /// current membership, never a stale snapshot. The pending selection's
    /// *name* is re-resolved against that fresh order (not trusted as a
    /// stale index) before advancing from it.
    ///
    /// Stale entries are excluded from the cycle target set entirely — every
    /// index/position below is computed against `selectable` (fresh entries
    /// only), never the full `order`. Alt+[/] must never land on a
    /// likely-dead session: `switch_session`ing onto one that's actually
    /// gone would have Zellij resurrect it as an empty zombie (task-14).
    pub(crate) fn cycle(&mut self, dir: Direction) -> bool {
        let before = self.badge();
        let order = self.ordered();
        let selectable: Vec<&OrderedEntry> = order.iter().filter(|s| !s.stale).collect();
        // A previously selected name that no longer appears among the fresh
        // entries (its session closed, or went stale itself) is treated the
        // same as "nothing selected" — restart below rather than advance
        // from a position it no longer occupies.
        let current_position = self
            .selection
            .as_ref()
            .and_then(|sel| selectable.iter().position(|s| s.presence.session_name == sel.name));
        self.selection = match (current_position, selectable.len()) {
            (_, 0) => None,
            (None, _) => {
                // Nothing selected yet (or the selection vanished): start at
                // the first non-current entry (index 0 is the current
                // session, when own is known).
                selectable.iter().position(|s| !s.is_current).map(|index| SelectionState {
                    name: selectable[index].presence.session_name.clone(),
                    taps_since_last_fire: true,
                })
            }
            (Some(index), len) => {
                let next = match dir {
                    Direction::Next => (index + 1) % len,
                    Direction::Prev => (index + len - 1) % len,
                };
                Some(SelectionState { name: selectable[next].presence.session_name.clone(), taps_since_last_fire: true })
            }
        };
        self.badge() != before
    }

    /// Idle-commit: called on every timer fire while a selection is pending.
    /// A fire whose covered interval saw a tap (`taps_since_last_fire` set —
    /// by `cycle()`, possibly including THIS very fire's own tap) clears the
    /// flag and skips: the deadline resets, and only the NEXT, fully quiet
    /// fire may commit. The selected *name* is re-resolved against a
    /// freshly derived order rather than a remembered index, so a reorder
    /// between the tap and this tick can never retarget a different
    /// session; if the name is no longer live, the selection is dropped and
    /// nothing commits. Landing on the current session is the cancel
    /// gesture (`None`, no effect); a selection that went stale while
    /// pending (its heartbeat lapsed between the tap and this fire) is
    /// dropped the same way — `cycle()` never selects a stale entry, so one
    /// showing up stale here can only mean it lapsed mid-flight, exactly the
    /// likely-dead session a commit must not switch onto. Either way a
    /// commit (or drop) clears the pending selection.
    pub(crate) fn tick(&mut self) -> Option<CommitTarget> {
        let sel = self.selection.as_mut()?;
        if sel.taps_since_last_fire {
            sel.taps_since_last_fire = false;
            return None;
        }
        let name = sel.name.clone();
        self.selection = None; // committing, cancelling, or dropping a vanished/staled selection all clear
        let order = self.ordered();
        let entry = order.iter().find(|s| s.presence.session_name == name)?; // vanished session: nothing to commit
        if entry.is_current || entry.stale {
            return None; // cancel gesture, or the selection lapsed into staleness before commit
        }
        Some(CommitTarget {
            name: entry.presence.session_name.clone(),
            attention_tab_position: entry.presence.attention_tab_position,
        })
    }

    /// The ordered badge: current session first, then fresh attention>0
    /// sessions by name, then the rest of the fresh sessions by name, then
    /// every stale session by name. Derived fresh from `peers`/`own` every
    /// call — never cached — so it can never drift from its inputs.
    pub(crate) fn badge(&self) -> Vec<BadgeEntry> {
        let order = self.ordered();
        let selected_name = self.selection.as_ref().map(|sel| sel.name.as_str());
        order
            .into_iter()
            .map(|s| BadgeEntry {
                name: s.presence.session_name.clone(),
                running: s.presence.running,
                attention: s.presence.attention,
                attention_tab_position: s.presence.attention_tab_position,
                is_current: s.is_current,
                selected: selected_name == Some(s.presence.session_name.as_str()),
                stale: s.stale,
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

    /// The shared ordering: the current session first (at most one — `own`,
    /// once known), then fresh attention>0 peers by name, then the rest of
    /// the fresh peers by name, then every stale peer by name — a stale
    /// peer's attention count is never actionable (it can't be cycled to),
    /// so staleness outranks attention for ordering purposes. Both
    /// `badge()` and `cycle()` go through this so "what's shown" and "what
    /// Alt+[/] steps through" can never disagree.
    fn ordered(&self) -> Vec<OrderedEntry<'_>> {
        let mut attention: Vec<OrderedEntry> = Vec::new();
        let mut rest: Vec<OrderedEntry> = Vec::new();
        let mut stale: Vec<OrderedEntry> = Vec::new();
        let own_name = self.own.as_ref().map(|p| p.session_name.as_str());
        for peer in &self.peers {
            // WHY: a peer presence sharing our own session's name is
            // necessarily either a stale corpse of this very session (pid-
            // keyed presence files: a restarted session under the same name
            // can coexist with a peer-visible copy of its old self) or a
            // race between reading peer files and set_own. `own`'s counts
            // come from set_own's direct knowledge, never from a peer file
            // (see its doc comment) — it must always win a name collision,
            // so the colliding peer is dropped outright rather than shown
            // as a second line for the same name.
            if own_name == Some(peer.presence.session_name.as_str()) {
                continue;
            }
            let entry = OrderedEntry { presence: &peer.presence, is_current: false, stale: peer.stale };
            if peer.stale {
                stale.push(entry);
            } else if peer.presence.attention > 0 {
                attention.push(entry);
            } else {
                rest.push(entry);
            }
        }
        attention.sort_by(|a, b| a.presence.session_name.cmp(&b.presence.session_name));
        rest.sort_by(|a, b| a.presence.session_name.cmp(&b.presence.session_name));
        stale.sort_by(|a, b| a.presence.session_name.cmp(&b.presence.session_name));
        let current = self.own.as_ref().map(|p| OrderedEntry { presence: p, is_current: true, stale: false });
        current.into_iter().chain(attention).chain(rest).chain(stale).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radar_state::Direction;

    fn own(name: &str) -> Presence {
        Presence { session_name: name.into(), running: 0, attention: 0,
                  attention_tab_position: None, updated_epoch_s: 0 }
    }
    /// Fresh (age 0) peer presence, the shape most tests want.
    fn presence(name: &str, running: usize, attention: usize) -> (String, u64) {
        presence_aged(name, running, attention, 0)
    }
    /// A peer presence paired with an explicit mtime age, for exercising
    /// the fresh/stale boundary (`STALE_AFTER_SECS`).
    fn presence_aged(name: &str, running: usize, attention: usize, age_secs: u64) -> (String, u64) {
        (format!(r#"{{"session_name":"{name}","running":{running},"attention":{attention}}}"#), age_secs)
    }

    #[test]
    fn badge_orders_current_then_attention_then_rest() {
        let mut s = Sessions::default();
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
    fn corrupt_presence_lines_are_skipped_leniently() {
        // Liveness is no longer cross-checked against a separate live list
        // here — `session_files::read_peer_presences`'s mtime gate now owns
        // "is this peer still alive?" (see its own tests). What's left for
        // this module to guard is `Presence::parse`'s leniency actually
        // getting exercised by its only caller: a malformed line among
        // otherwise-valid ones must be dropped, not choke the whole batch.
        let mut s = Sessions::default();
        s.update_presences(vec![("not json".to_string(), 0), presence("alpha", 1, 0)]);
        let badge = s.badge();
        let names: Vec<&str> = badge.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
    }

    #[test]
    fn cycle_advances_and_wraps_and_tick_commits_after_idle() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 0, 1), presence("beta", 1, 0)]);
        // Order: work(current), alpha(attention), beta.
        s.cycle(Direction::Next);                     // select alpha
        assert_eq!(s.tick(), None, "the fire covering the tap must not commit — it clears the flag");
        s.cycle(Direction::Next);                     // skip to beta
        assert_eq!(s.tick(), None, "the fire covering THIS tap must not commit either");
        let t = s.tick().expect("a fully quiet fire commits");
        assert_eq!(t.name, "beta");
        assert_eq!(s.tick(), None, "committed selection is cleared");
    }

    #[test]
    fn committing_on_current_session_is_a_noop_cancel() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 0, 0)]);
        s.cycle(Direction::Next);                     // alpha
        s.cycle(Direction::Next);                     // wraps to work (current)
        s.tick();                                     // the fire covering that tap: skip, clears the flag
        assert_eq!(s.tick(), None, "a quiet fire landing on the current session cancels");
    }

    #[test]
    fn commit_carries_attention_tab_position() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![
            (r#"{"session_name":"alpha","running":0,"attention":1,"attention_tab_position":2}"#.to_string(), 0),
        ]);
        s.cycle(Direction::Next);
        s.tick(); // the fire covering the tap: skip, clears the flag
        assert_eq!(s.tick().unwrap().attention_tab_position, Some(2));
    }

    // -- Pinning: field bugs in the old tick-counter commit check ------------
    // The old `now_tick > last_tap_tick` comparison committed on the very
    // next fire after a tap (a tap landing just before an already-scheduled
    // fire compared as "idle" and committed instantly) and never extended
    // the deadline for taps faster than the 1s tick grain (they all shared
    // one tick number, so the commit landed ~1s after the FIRST tap, not
    // the last). The `taps_since_last_fire` flag fixes both.

    #[test]
    fn tap_then_immediate_fire_does_not_commit() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 1, 0)]);
        s.cycle(Direction::Next); // selects alpha
        // A fire landing right after the tap must not commit instantly —
        // it covers the tap's interval and must reset the deadline instead.
        assert_eq!(s.tick(), None, "a fire landing right after the tap must not commit instantly");
        assert!(s.wants_fast_cadence(), "the selection survives that fire, still pending");
        let t = s.tick().expect("the next, fully quiet fire commits");
        assert_eq!(t.name, "alpha");
    }

    #[test]
    fn rapid_multi_tap_across_several_fires_commits_only_after_first_quiet_fire() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 1, 0), presence("beta", 1, 0)]);
        // Order: work(current), alpha, beta. Several taps land, each one
        // arriving before the next fire — faster than the old 1-tick grain
        // could distinguish. Every fire covering a tap must skip, not just
        // the first.
        s.cycle(Direction::Next); // alpha
        assert_eq!(s.tick(), None);
        s.cycle(Direction::Next); // beta
        assert_eq!(s.tick(), None);
        s.cycle(Direction::Next); // wraps to work (current) — still just cycling
        assert_eq!(s.tick(), None, "a fire covering a tap must skip even on the 3rd consecutive tap");
        s.cycle(Direction::Prev); // back to beta — the LAST tap in this burst
        assert_eq!(s.tick(), None, "the fire covering the last tap still must not commit");
        let t = s.tick().expect("the first fully quiet fire after the burst commits");
        assert_eq!(t.name, "beta", "must commit to whatever the LAST tap selected, not an earlier one");
    }

    // -- Pinning: wants_fast_cadence() --------------------------------------
    // No wasm caller exercises this from a host test, so a refactor that
    // breaks the "armed while a selection is pending" contract would
    // otherwise go unnoticed until manual testing in Zellij.

    #[test]
    fn wants_fast_cadence_tracks_pending_selection_through_commit_and_cancel() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 1, 0), presence("beta", 1, 0)]);
        assert!(!s.wants_fast_cadence(), "nothing selected initially");

        s.cycle(Direction::Next); // selects alpha
        assert!(s.wants_fast_cadence(), "a cycle tap arms a pending selection");

        s.tick(); // the fire covering the tap: skip, still pending
        assert!(s.wants_fast_cadence(), "a tap-covering fire must not clear the pending selection");
        s.tick(); // idle commit to alpha
        assert!(!s.wants_fast_cadence(), "a commit clears the pending selection");

        s.cycle(Direction::Next); // alpha
        s.cycle(Direction::Next); // beta
        s.cycle(Direction::Next); // wraps to work (current)
        assert!(s.wants_fast_cadence(), "still pending until the idle tick fires");

        s.tick(); // the fire covering the last tap: skip, still pending
        assert!(s.wants_fast_cadence());
        s.tick(); // cancel-commit (lands on current)
        assert!(!s.wants_fast_cadence(), "a cancel-commit also clears the pending selection");
    }

    // -- Pinning: content-compare returns ------------------------------------
    // Each mutator must report `true` only when the derived badge actually
    // differs, not merely because it was called. A refactor that starts
    // returning `true` unconditionally (or `false` unconditionally) would
    // make the runtime over- or under-repaint without any other test
    // noticing, since none of the tests above assert on the return value.

    #[test]
    fn update_presences_reports_change_only_on_actual_content_change() {
        let mut s = Sessions::default();
        assert!(s.update_presences(vec![presence("alpha", 1, 0)]), "first presence report changes the badge");
        assert!(!s.update_presences(vec![presence("alpha", 1, 0)]), "identical presence report is not a change");
    }

    #[test]
    fn set_own_reports_change_only_on_actual_content_change() {
        let mut s = Sessions::default();
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
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 1, 0), presence("beta", 1, 0)]);
        assert!(s.cycle(Direction::Next), "starting a selection flips a `selected` flag in the badge");
        assert!(s.cycle(Direction::Next), "advancing the selection moves the `selected` flag");
    }

    #[test]
    fn dismiss_removes_peer_entry() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 2, 1)]);

        assert!(s.dismiss("alpha"), "dismissing an existing peer changes the badge");
        assert!(
            !s.badge().iter().any(|b| b.name == "alpha"),
            "dismissed peer must no longer appear on the badge"
        );
    }

    #[test]
    fn dismiss_absent_name_is_noop() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 2, 1)]);
        let before = s.badge();

        assert!(!s.dismiss("beta"), "dismissing a missing peer must report no badge change");
        assert_eq!(s.badge(), before, "missing-name dismiss must not disturb the badge");
    }

    #[test]
    fn dismissed_but_alive_session_reappears_fresh_on_next_presence_write() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence_aged("alpha", 2, 1, STALE_AFTER_SECS + 1)]);
        assert!(s.dismiss("alpha"), "setup: stale peer is dismissed locally");
        assert!(!s.badge().iter().any(|b| b.name == "alpha"));

        s.update_presences(vec![presence_aged("alpha", 2, 1, 0)]);

        let alpha = s.badge().into_iter().find(|b| b.name == "alpha").expect("fresh write makes alpha reappear");
        assert!(!alpha.stale, "a live session's fresh write must reappear as fresh, not stale");
    }

    // -- Selection identity (not positional index) ---------------------------
    // `SelectionState` must track the selected session by name, not by its
    // position in the derived order — a reorder between the cycle() tap and
    // the commit tick() must never silently retarget a different session.

    #[test]
    fn cycle_selection_survives_reordering_by_name_not_index() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 1, 0), presence("beta", 1, 0)]);
        // Order: work (current), alpha, beta — both zero-attention, by name.
        s.cycle(Direction::Next); // selects alpha (first non-current)

        // "aardvark" joins and sorts ahead of "alpha" in the rest bucket. A
        // positional-index selection (old index 1) would now point at
        // "aardvark" instead of "alpha". `update_presences` REPLACES the
        // peer set wholesale (same as `update_live` used to for `live`), so
        // this is the same "membership changed under the selection" shape.
        s.update_presences(vec![presence("aardvark", 1, 0), presence("alpha", 1, 0), presence("beta", 1, 0)]);

        s.tick(); // the fire covering the tap: skip, clears the flag
        let t = s.tick().expect("a quiet fire commits");
        assert_eq!(t.name, "alpha", "selection must follow the named session, not the reordered index");
    }

    #[test]
    fn selected_session_vanishing_before_tick_clears_selection_and_commits_nothing() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 0, 0)]);
        s.cycle(Direction::Next); // selects alpha
        s.update_presences(vec![]); // alpha's presence vanished before the idle tick

        s.tick(); // the fire covering the tap: skip, clears the flag (selection still pending)
        assert_eq!(s.tick(), None, "a vanished selection must not commit");
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
        s.set_own(own("work"));
        s.update_presences(vec![presence("alpha", 1, 0), presence("beta", 1, 0), presence("gamma", 1, 0)]);
        // No attention: order is work (current), then alpha, beta, gamma by name.

        s.cycle(Direction::Next); // selects alpha (first non-current)

        // alpha vanishes, leaving a fresh order with *two* non-current entries
        // (beta, gamma) — order: work, beta, gamma.
        s.update_presences(vec![presence("beta", 1, 0), presence("gamma", 1, 0)]);

        // Cycling with Prev is what actually discriminates restart-fresh from
        // an index-0 fallback: restart-fresh ignores the stale direction and
        // always selects the first non-current entry (beta). A regression
        // that resolved the vanished name to index 0 ("as if `work`/current
        // were selected") would instead advance *backward* from there and
        // land on the last entry (gamma) — the same bug that, under
        // Direction::Next, would coincidentally land on beta too and pass
        // silently.
        s.cycle(Direction::Prev);
        let badge = s.badge();
        let selected: Vec<&str> = badge.iter().filter(|b| b.selected).map(|b| b.name.as_str()).collect();
        assert_eq!(
            selected,
            vec!["beta"],
            "restart-fresh must ignore the stale direction and pick the first \
             non-current entry, not fall back to advancing from index 0"
        );

        s.tick(); // the fire covering the last tap: skip, clears the flag
        let t = s.tick().expect("a quiet fire commits");
        assert_eq!(t.name, "beta", "after vanished selection, cycle must restart and select the first non-current entry");
    }

    // -- Pinning: duplicate-name dedup (killed-then-recreated session) ------
    // Presence files are pid-keyed on disk, not name-keyed: a session killed
    // and recreated under the same name leaves its old presence file (a
    // "corpse") coexisting with the fresh one until the open-time sweep
    // reaps it, up to `PRESENCE_MAX_AGE` (6h) later. Without a dedup,
    // `badge()` would render the same name twice for that whole window —
    // exactly the bug the user observed.

    #[test]
    fn update_presences_dedupes_by_name_keeping_freshest() {
        let mut s = Sessions::default();
        let corpse = r#"{"session_name":"alpha","running":1,"attention":0,"updated_epoch_s":10}"#;
        let live = r#"{"session_name":"alpha","running":5,"attention":2,"updated_epoch_s":20}"#;
        s.update_presences(vec![(corpse.to_string(), 0), (live.to_string(), 0)]);
        let badge = s.badge();
        let alphas: Vec<&BadgeEntry> = badge.iter().filter(|b| b.name == "alpha").collect();
        assert_eq!(alphas.len(), 1, "two presences for the same name must collapse to one badge entry");
        assert_eq!(alphas[0].running, 5, "the surviving entry must carry the fresher (greater updated_epoch_s) counts");
        assert_eq!(alphas[0].attention, 2, "the surviving entry must carry the fresher (greater updated_epoch_s) counts");
    }

    #[test]
    fn peer_presence_claiming_own_name_does_not_duplicate_own_entry() {
        // A killed-then-recreated session's corpse can also collide with
        // OUR OWN name specifically (the pid changed, the name didn't).
        // `own` is this session's own direct knowledge (set_own's doc
        // comment: never read back from disk) and must win outright, not
        // merely "whichever is fresher" — the peer here claims a higher
        // updated_epoch_s and larger counts, and must still lose.
        let mut s = Sessions::default();
        s.set_own(Presence { session_name: "work".into(), running: 3, attention: 0,
                             attention_tab_position: None, updated_epoch_s: 50 });
        s.update_presences(vec![
            (r#"{"session_name":"work","running":9,"attention":9,"updated_epoch_s":999}"#.to_string(), 0),
        ]);
        let badge = s.badge();
        let work_entries: Vec<&BadgeEntry> = badge.iter().filter(|b| b.name == "work").collect();
        assert_eq!(work_entries.len(), 1, "a peer claiming our own name must not add a second badge line");
        assert!(work_entries[0].is_current, "the surviving line must be the own entry");
        assert_eq!(work_entries[0].running, 3, "own's own-known counts must win over a peer claiming the same name");
        assert_eq!(work_entries[0].attention, 0, "own's own-known counts must win over a peer claiming the same name");
    }

    // -- Pinning: persistent roster (task-14) --------------------------------
    // A remembered session must never silently vanish from the badge. Peers
    // past `STALE_AFTER_SECS` mark stale (dimmed by the renderer, unreachable
    // via `cycle()`) instead of disappearing; only the open-time
    // `PRESENCE_MAX_AGE` sweep (6h) actually forgets one.

    #[test]
    fn stale_peer_is_flagged_and_skipped_by_cycle() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence_aged("alpha", 1, 0, 400)]); // well past STALE_AFTER_SECS
        let badge = s.badge();
        let alpha = badge.iter().find(|b| b.name == "alpha").expect("stale peer still on the badge");
        assert!(alpha.stale, "a peer whose mtime age exceeds STALE_AFTER_SECS must flag stale");

        // The ONLY peer is stale — cycle() must find nothing selectable and
        // stay a no-op, never landing on the likely-dead session.
        let changed = s.cycle(Direction::Next);
        assert!(!changed, "cycle must not select a stale-only peer set");
        assert!(!s.wants_fast_cadence(), "no selection is armed when nothing selectable exists");
    }

    #[test]
    fn cycle_skips_a_stale_peer_and_lands_on_a_fresh_one_instead() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence_aged("alpha", 1, 0, 400), presence("beta", 1, 0)]);
        // Order: work (current), beta (fresh, before the stale bucket), alpha (stale, last).
        s.cycle(Direction::Next);
        let badge = s.badge();
        let selected: Vec<&str> = badge.iter().filter(|b| b.selected).map(|b| b.name.as_str()).collect();
        assert_eq!(selected, vec!["beta"], "cycle must skip the stale peer entirely and select the fresh one");
    }

    #[test]
    fn fresh_to_stale_transition_flips_on_age() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence_aged("alpha", 1, 0, 10)]); // fresh
        assert!(!s.badge()[1].stale, "a recently-heartbeated peer must not start stale");

        // The next read finds the same peer with an older mtime age (no
        // further heartbeat landed) — its badge row must flip to stale.
        s.update_presences(vec![presence_aged("alpha", 1, 0, 200)]);
        assert!(s.badge()[1].stale, "a peer whose age crossed STALE_AFTER_SECS must flip to stale");
    }

    #[test]
    fn stale_entry_superseded_by_a_fresh_same_name_presence_undims() {
        // The recreation case: a session dies (its old file ages past
        // STALE_AFTER_SECS) and gets recreated under the same name — a
        // fresh presence file, young mtime, higher `updated_epoch_s`. The
        // corpse and the recreation can even coexist in the SAME read
        // (different pids, same name); dedup must pick the fresh one AND
        // its (fresh) staleness, not the corpse's.
        let mut s = Sessions::default();
        s.set_own(own("work"));
        let corpse = r#"{"session_name":"alpha","running":1,"attention":0,"updated_epoch_s":10}"#;
        let recreated = r#"{"session_name":"alpha","running":0,"attention":0,"updated_epoch_s":20}"#;
        s.update_presences(vec![(corpse.to_string(), 400), (recreated.to_string(), 0)]);
        let alpha = s.badge().into_iter().find(|b| b.name == "alpha").expect("alpha present");
        assert!(!alpha.stale, "the fresher recreation must win the dedup, undimming the entry");
    }

    #[test]
    fn roster_survives_beyond_the_old_liveness_ttl() {
        // The old design would have had `session_files::read_peer_presences`
        // itself drop a peer at 180s. Task-14: it now returns unconditionally,
        // and this module marks stale rather than dropping — an entry with
        // age 400s (well past the old TTL) must still be on the badge.
        let mut s = Sessions::default();
        s.set_own(own("work"));
        s.update_presences(vec![presence_aged("alpha", 1, 0, 400)]);
        let badge = s.badge();
        assert!(badge.iter().any(|b| b.name == "alpha"), "a peer must never be dropped for being old — only marked stale");
    }

    #[test]
    fn cycle_returns_false_on_noop_with_only_current_session() {
        let mut s = Sessions::default();
        s.set_own(own("work"));
        // No presences, no peers — work is the only session.

        let changed = s.cycle(Direction::Next);
        assert!(!changed, "cycle with nothing to select must return false (no badge change)");
        assert!(!s.wants_fast_cadence(), "cycle with nothing to select must not set up a pending selection");
    }
}
