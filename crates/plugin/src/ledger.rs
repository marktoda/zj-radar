//! The completion ledger: a small ring of "what finished earlier" entries that
//! cards hand off to when a Done/Error leaves the rail (spec §4). Pure data +
//! policy: `RadarState` feeds it recede edges (wired in `radar_state.rs`); the
//! renderer consumes prepared lines. Convergent across instances because every
//! entry edge is a shared signal and the snapshot merges rings.
//!
//! Consumers, by seam: `push` via `RadarState::ledger_receded` (every edge
//! that retires a card); `to_vec`/`replace`/`merge` via snapshot persistence
//! (`RadarState::snapshot_json`/`load_snapshot`); `entries`/`format_age` via
//! the rail's bottom region (`render::render_bottom` through
//! `RadarState::ledger_lines`); `any_unsaturated`/`SATURATE_S` via
//! `PluginRuntime::desired_cadence` (the Slow-vs-disarm cadence pick); and
//! `is_empty` via `PluginRuntime::render`'s onboarding-vs-rail choice.

use crate::observation::{ObservationOrigin, TrackedObservation};
use crate::payload::{sanitize, MAX_MSG_CHARS, MAX_TAB_NAME_CHARS};
use crate::radar_state::TabId;
use crate::status::Status;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Storage cap for the ring — bounds the snapshot and the merge work, NOT what
/// the rail shows (`render::LEDGER_DISPLAY_CAP` caps that at 10 rows).
pub(crate) const LEDGER_CAP: usize = 32;
/// Two entries within this many seconds and equal (pane, outcome, label) are
/// the same event observed by different instances (spec §4.3).
pub(crate) const MERGE_WINDOW_S: u64 = 4;
/// Ages ≥ this render as the frozen "1h+" — the display never changes again,
/// which is what lets the idle timer fully disarm (spec §4.4/§10).
pub(crate) const SATURATE_S: u64 = 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum LedgerOutcome {
    Done,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LedgerEntry {
    pub at_epoch_s: u64,
    pub outcome: LedgerOutcome,
    pub tab_id: TabId,
    pub tab_name: String,
    pub label: String,
    pub pane_id: u32,
}

impl LedgerEntry {
    /// Build an entry from an observation leaving the card. `None` unless the
    /// observation is a stamped completion (Done/Error with a completion epoch —
    /// an unstamped one can only come from a pre-v3 snapshot; skipping it is the
    /// accepted transient, spec §4.3).
    pub(crate) fn from_observation(
        pane_id: u32,
        obs: &TrackedObservation,
        tab_id: TabId,
        tab_name: &str,
    ) -> Option<LedgerEntry> {
        let outcome = match obs.status {
            Status::Done => LedgerOutcome::Done,
            Status::Error => LedgerOutcome::Error,
            _ => return None,
        };
        let at_epoch_s = obs.completed_epoch_s?;
        // An empty msg still needs a row identity — a ledger line never reads
        // blank. Agent turns fall back to "turn done"; a command completion
        // with no recorded command string (an exit that beat every
        // `CommandChanged` edge) falls back to its kind's own token
        // ("command", "test", …), the same identity the card's mark carries.
        let label = if obs.msg.trim().is_empty() {
            match obs.origin {
                ObservationOrigin::Command => obs.kind.as_source().to_string(),
                ObservationOrigin::StatusPipe => "turn done".to_string(),
            }
        } else {
            obs.msg.clone()
        };
        Some(LedgerEntry { at_epoch_s, outcome, tab_id, tab_name: sanitized_or(tab_name), label, pane_id })
    }

    /// Re-scrub the free-text fields. Live entries are built from
    /// already-sanitized state; this is for entries loaded off disk, where a
    /// pre-sanitize build (or a hand-edited snapshot) may have persisted raw
    /// control chars that would otherwise reach the render grid.
    pub(crate) fn sanitized(mut self) -> LedgerEntry {
        self.tab_name = sanitized_or(&self.tab_name);
        self.label = sanitize(&self.label, MAX_MSG_CHARS);
        self
    }
}

/// Sanitize (strip controls/ANSI, cap like a tab name) + fall back to `"tab"`
/// for an empty result, so a ledgered entry never shows a blank tab column.
fn sanitized_or(name: &str) -> String {
    let clean = sanitize(name, MAX_TAB_NAME_CHARS);
    let trimmed = clean.trim();
    if trimmed.is_empty() {
        "tab".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Same (pane, outcome, label) within `MERGE_WINDOW_S` — the nearest-neighbor
/// match shared by `push` and `merge` (spec §4.3). Symmetric in its two
/// arguments.
fn is_same_event(a: &LedgerEntry, b: &LedgerEntry) -> bool {
    a.pane_id == b.pane_id
        && a.outcome == b.outcome
        && a.label == b.label
        && a.at_epoch_s.abs_diff(b.at_epoch_s) <= MERGE_WINDOW_S
}

#[derive(Default)]
pub(crate) struct Ledger {
    entries: VecDeque<LedgerEntry>,
} // newest at front

impl Ledger {
    /// Append unless a matching entry (same pane/outcome/label within
    /// MERGE_WINDOW_S) is already present — the dedup guard that stops
    /// overlapping recede edges (prune racing TTL) double-appending.
    ///
    /// Deliberately asymmetric with `merge` on which stamp survives a match:
    /// `push` keeps the EXISTING entry's stamp (the duplicate is this instance
    /// re-observing its own local edge, so first-seen is the honest completion
    /// time), while `merge` keeps the LATER stamp (two instances stamped the
    /// same event independently; the later one is the fresher knowledge). Safe
    /// because a match by definition lies within MERGE_WINDOW_S: whichever
    /// stamp wins, rendered ages differ by ≤4s and re-merging still matches.
    pub(crate) fn push(&mut self, entry: LedgerEntry) -> bool {
        if self.entries.iter().any(|existing| is_same_event(existing, &entry)) {
            return false;
        }
        self.entries.push_front(entry);
        self.entries.truncate(LEDGER_CAP);
        true
    }

    /// newest first
    pub(crate) fn entries(&self) -> impl Iterator<Item = &LedgerEntry> {
        self.entries.iter()
    }

    /// Consumed (via `RadarState::ledger_is_empty`) by `PluginRuntime::render`:
    /// zero tracked tabs AND an empty ledger is the only state that shows the
    /// onboarding face instead of the rail.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn to_vec(&self) -> Vec<LedgerEntry> {
        self.entries.iter().cloned().collect()
    }

    /// sorted desc, capped
    pub(crate) fn replace(&mut self, entries: Vec<LedgerEntry>) {
        let mut sorted = entries;
        sorted.sort_by_key(|e| std::cmp::Reverse(e.at_epoch_s));
        sorted.truncate(LEDGER_CAP);
        self.entries = sorted.into();
    }

    /// Any entry still younger than SATURATE_S? (Drives the Slow cadence.)
    pub(crate) fn any_unsaturated(&self, now_epoch_s: u64) -> bool {
        self.entries.iter().any(|e| now_epoch_s.saturating_sub(e.at_epoch_s) < SATURATE_S)
    }

    /// Union of two rings: nearest-neighbor match on (pane, outcome, label)
    /// within MERGE_WINDOW_S keeps the later stamp; result sorted by
    /// at_epoch_s desc, truncated to LEDGER_CAP.
    pub(crate) fn merge(a: Vec<LedgerEntry>, b: Vec<LedgerEntry>) -> Vec<LedgerEntry> {
        let mut all = a;
        all.extend(b);
        all.sort_by_key(|e| std::cmp::Reverse(e.at_epoch_s));
        let mut kept: Vec<LedgerEntry> = Vec::with_capacity(all.len());
        for candidate in all {
            if !kept.iter().any(|k| is_same_event(k, &candidate)) {
                kept.push(candidate);
            }
        }
        kept.truncate(LEDGER_CAP);
        kept
    }
}

/// Relative age per the spec §4.4 table. Negative (clock skew) → "<1m".
///
/// The final band starts at SATURATE_S so the rendered age stops changing
/// exactly when `any_unsaturated` goes false and the Slow timer disarms —
/// a frozen "1h+" is what makes full disarm safe.
pub(crate) fn format_age(at_epoch_s: u64, now_epoch_s: u64) -> String {
    let age = now_epoch_s.saturating_sub(at_epoch_s);
    if age < 60 {
        "<1m".to_string()
    } else if age < SATURATE_S {
        format!("{}m", age / 60)
    } else {
        "1h+".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(status: Status, origin: ObservationOrigin, msg: &str, completed_epoch_s: Option<u64>) -> TrackedObservation {
        TrackedObservation {
            origin,
            status,
            repo: "repo".into(),
            branch: "main".into(),
            msg: msg.to_string(),
            task: String::new(),
            kind: crate::kind::Kind::Claude,
            last_change_tick: 1,
            ever_active: true,
            exit_code: None,
            completed_epoch_s,
            pending_epoch_s: None,
            acknowledged: false,
        }
    }

    fn entry(pane_id: u32, outcome: LedgerOutcome, label: &str, at_epoch_s: u64) -> LedgerEntry {
        LedgerEntry { at_epoch_s, outcome, tab_id: TabId::new(0), tab_name: "tab".into(), label: label.to_string(), pane_id }
    }

    #[test]
    fn sanitized_scrubs_disk_loaded_free_text() {
        // Entries loaded off disk may predate intake sanitization; the scrub
        // strips ANSI/controls and folds newlines like every other intake.
        let e = entry(1, LedgerOutcome::Done, "did\nthings\x1b[31m", 5);
        let e = LedgerEntry { tab_name: "ev\x1b[2Jil\ntab".into(), ..e }.sanitized();
        assert_eq!(e.tab_name, "evil tab");
        assert_eq!(e.label, "did things");
    }

    #[test]
    fn from_observation_maps_command_and_agent_labels() {
        // Command-origin: msg passes through untouched.
        let o = obs(Status::Done, ObservationOrigin::Command, "cargo test", Some(100));
        let e = LedgerEntry::from_observation(1, &o, TabId::new(0), "work").unwrap();
        assert_eq!(e.label, "cargo test");
        assert_eq!(e.outcome, LedgerOutcome::Done);
        assert_eq!(e.tab_name, "work");
        assert_eq!(e.at_epoch_s, 100);
        assert_eq!(e.pane_id, 1);

        // Command-origin with an empty message falls back to the kind's own
        // token — never a blank ledger row.
        let mut o = obs(Status::Error, ObservationOrigin::Command, "  ", Some(150));
        o.kind = crate::kind::Kind::Command;
        let e = LedgerEntry::from_observation(6, &o, TabId::new(0), "work").unwrap();
        assert_eq!(e.label, "command");
        let mut o = obs(Status::Done, ObservationOrigin::Command, "", Some(160));
        o.kind = crate::kind::Kind::Test;
        let e = LedgerEntry::from_observation(7, &o, TabId::new(0), "work").unwrap();
        assert_eq!(e.label, "test");

        // Agent-origin (status pipe) with an empty message falls back to
        // "turn done".
        let o = obs(Status::Error, ObservationOrigin::StatusPipe, "   ", Some(200));
        let e = LedgerEntry::from_observation(2, &o, TabId::new(1), "").unwrap();
        assert_eq!(e.label, "turn done");
        assert_eq!(e.outcome, LedgerOutcome::Error);
        // Empty tab name sanitizes to the "tab" fallback.
        assert_eq!(e.tab_name, "tab");

        // Agent-origin with a real message passes it through.
        let o = obs(Status::Done, ObservationOrigin::StatusPipe, "reviewed PR", Some(300));
        let e = LedgerEntry::from_observation(3, &o, TabId::new(0), "work").unwrap();
        assert_eq!(e.label, "reviewed PR");

        // Running (not a completion) never ledgers, even with a stamp.
        let o = obs(Status::Running, ObservationOrigin::StatusPipe, "busy", Some(400));
        assert!(LedgerEntry::from_observation(4, &o, TabId::new(0), "work").is_none());

        // Done without a completion stamp (pre-v3 snapshot transient) skips too.
        let o = obs(Status::Done, ObservationOrigin::Command, "cargo build", None);
        assert!(LedgerEntry::from_observation(5, &o, TabId::new(0), "work").is_none());
    }

    #[test]
    fn push_dedups_within_window_and_caps_at_32() {
        let mut ledger = Ledger::default();
        assert!(ledger.is_empty());

        assert!(ledger.push(entry(1, LedgerOutcome::Done, "cargo test", 100)));
        // Same (pane, outcome, label) 2s later is the same event — rejected.
        assert!(!ledger.push(entry(1, LedgerOutcome::Done, "cargo test", 102)));
        assert_eq!(ledger.entries().count(), 1);
        assert_eq!(ledger.to_vec()[0].at_epoch_s, 100, "the original stamp is kept, not the dup's");

        // A different label on the same pane is a distinct event.
        assert!(ledger.push(entry(1, LedgerOutcome::Done, "cargo build", 103)));
        assert_eq!(ledger.entries().count(), 2);

        // 40 distinct pushes (far enough apart, or on distinct panes, to all
        // survive dedup) cap the ring at 32, keeping the newest.
        let mut ledger = Ledger::default();
        for i in 0..40u32 {
            assert!(ledger.push(entry(i, LedgerOutcome::Done, "cargo test", 1000 + i as u64 * 10)));
        }
        assert_eq!(ledger.entries().count(), LEDGER_CAP);
        let newest = ledger.entries().next().unwrap();
        assert_eq!(newest.pane_id, 39, "newest push stays at the front");
        assert!(ledger.entries().all(|e| e.pane_id >= 8), "the oldest 8 were evicted");
    }

    #[test]
    fn merge_is_idempotent_and_dedups_across_boundaries() {
        // 1s-apart stamps MUST merge even at any absolute value (nearest-
        // neighbor, not fixed buckets): entries at 7999 and 8001 collapse to
        // one, keeping the later (8001) stamp.
        let a = vec![entry(1, LedgerOutcome::Done, "x", 7999)];
        let b = vec![entry(1, LedgerOutcome::Done, "x", 8001)];
        let merged = Ledger::merge(a, b);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].at_epoch_s, 8001);

        // merge(a, a) == a for an already-deduped, sorted-desc ring.
        let a = vec![entry(1, LedgerOutcome::Done, "x", 300), entry(2, LedgerOutcome::Error, "y", 100)];
        assert_eq!(Ledger::merge(a.clone(), a.clone()), a);

        // Distinct completions more than MERGE_WINDOW_S apart both survive.
        let a = vec![entry(1, LedgerOutcome::Done, "x", 100)];
        let b = vec![entry(1, LedgerOutcome::Done, "x", 105)];
        let merged = Ledger::merge(a, b);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].at_epoch_s, 105);
        assert_eq!(merged[1].at_epoch_s, 100);
    }

    #[test]
    fn merge_collapses_identical_completions_within_4s_by_design() {
        // The accepted artifact, pinned so it's deliberate: two genuinely
        // distinct completions that happen to share (pane, outcome, label)
        // within the merge window collapse into one row. Spec §4.3 accepts
        // this rather than growing a per-completion identity.
        let a = vec![entry(1, LedgerOutcome::Done, "cargo test", 1000)];
        let b = vec![entry(1, LedgerOutcome::Done, "cargo test", 1004)];
        let merged = Ledger::merge(a, b);
        assert_eq!(merged, vec![entry(1, LedgerOutcome::Done, "cargo test", 1004)]);
    }

    #[test]
    fn replace_sorts_desc_and_caps() {
        let mut ledger = Ledger::default();
        let unsorted = vec![
            entry(1, LedgerOutcome::Done, "a", 50),
            entry(2, LedgerOutcome::Done, "b", 200),
            entry(3, LedgerOutcome::Done, "c", 100),
        ];
        ledger.replace(unsorted);
        let got: Vec<u64> = ledger.entries().map(|e| e.at_epoch_s).collect();
        assert_eq!(got, vec![200, 100, 50]);

        let oversized: Vec<LedgerEntry> =
            (0..40u32).map(|i| entry(i, LedgerOutcome::Done, "x", i as u64)).collect();
        ledger.replace(oversized);
        assert_eq!(ledger.to_vec().len(), LEDGER_CAP);
    }

    #[test]
    fn any_unsaturated_reflects_age() {
        let ledger = Ledger::default();
        assert!(!ledger.any_unsaturated(1_000_000), "an empty ledger has nothing left to age out");

        let mut ledger = Ledger::default();
        ledger.push(entry(1, LedgerOutcome::Done, "x", 1000));
        assert!(ledger.any_unsaturated(1000 + SATURATE_S - 1));
        assert!(!ledger.any_unsaturated(1000 + SATURATE_S));
    }

    #[test]
    fn age_format_table() {
        assert_eq!(format_age(100, 100), "<1m");
        assert_eq!(format_age(100, 159), "<1m");
        assert_eq!(format_age(100, 160), "1m");
        assert_eq!(format_age(100, 100 + 59 * 60), "59m");
        assert_eq!(format_age(100, 100 + 3600), "1h+");
        assert_eq!(format_age(200, 100), "<1m"); // negative skew
    }
}
