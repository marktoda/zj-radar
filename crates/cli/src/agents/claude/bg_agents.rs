//! Background subagents launched from a pane, so their own hooks can be
//! dropped before they clobber the parent's row.
//!
//! A subagent's tool calls fire Pre/PostToolUse on the parent's pane, stamped
//! with its `agent_id` (verified live on Claude Code 2.1.282; the parent's
//! own hooks never carry one), and its end fires `SubagentStop` with the same
//! id. For a *background* subagent these arrive after the parent's turn
//! ended, so as plain running payloads they overwrote "waiting on …" and
//! downgraded a question-`needs you` to running before the user answered. A
//! *foreground* subagent's hooks must keep reporting: its PostToolUse is the
//! Pending-recovery edge after a permission answered inside it. The hooks
//! can't tell the two apart, but the launch can: the parent's `Agent`
//! PostToolUse answers a background launch with `{status: "async_launched",
//! agentId}`, and that `agentId` is the same string as the subagent's hook
//! `agent_id` (verified live). So a launch leaves one marker file per id,
//! `bg-agents.<session>.<pane>.<id>`, and a Pre/PostToolUse or `SubagentStop`
//! whose `agent_id` has a live marker sends nothing (`SubagentStop` also
//! removes it). The task-notification wake that follows a background
//! subagent's end reports its outcome instead.
//!
//! One file per id, not one record: parallel launches (two `Agent` calls in
//! one turn) each create their own file, so no read-modify-write can lose an
//! id. A marker's mtime is its age: past [`TTL_SECS`] it is treated as absent
//! and unlinked (launches also sweep the pane's stale markers, which bounds
//! the directory), and the pane's `SessionStart`/`SessionEnd` (the `idle`
//! edges) remove them all. Any IO failure fails open — the hook reports, as
//! it did before this existed. CLI-only: notify.sh's bash fallback keeps no
//! state and reports every subagent hook (parity.bats documents it).
//!
//! Known limit: background subagents surface permission prompts in the main
//! session (Claude Code docs), so one can raise `needs you`, and its
//! PostToolUse — the recovery edge once answered — is dropped here. The row
//! stays `needs you` until the parent's next hook or the wake when that
//! subagent ends. Letting a background PostToolUse through whenever the last
//! sent status was pending would re-open the bug this module exists for: a
//! pending from the parent's own trailing question or permission prompt looks
//! the same, so its tool calls would clear a `needs you` nobody answered.

use crate::dedup::{sanitize, state_dir};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

/// A marker older than this is forgotten even without its `SubagentStop` (a
/// crashed session, a missed hook): no subagent runs for hours, and a stale
/// marker can only suppress hooks carrying that exact random id.
pub const TTL_SECS: u64 = 6 * 60 * 60;
/// Longest id kept in a marker's filename (live ids are 17 chars).
const MAX_ID_CHARS: usize = 64;

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Cheap pre-filter on the raw payload: only these hooks can create, test or
/// remove a marker, so every other hook (the common case: the parent's own
/// tool calls) skips the state dir, the JSON parse and the file IO. The
/// caller runs it before [`BgAgents::from_env`].
pub(crate) fn relevant(raw: &str) -> bool {
    ["\"agent_id\"", "async_launched", "\"SessionStart\"", "\"SessionEnd\""].iter().any(|n| raw.contains(n))
}

/// A filename-safe id (`[A-Za-z0-9_-]`, capped), or `None` if nothing is left.
fn file_id(id: &str) -> Option<String> {
    let safe: String = id
        .chars()
        .take(MAX_ID_CHARS)
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
        .collect();
    safe.chars().any(|c| c != '_').then_some(safe)
}

/// One pane's background-subagent markers.
pub(crate) struct BgAgents {
    dir: PathBuf,
    /// `bg-agents.<session>.<pane>.` — every marker of this pane starts so.
    prefix: String,
}

impl BgAgents {
    /// The markers for `pane_id` in the current Zellij session, or `None`
    /// without a session identity to scope them by, or without a state dir
    /// that is provably ours (nothing is suppressed).
    pub fn from_env(pane_id: u32) -> Option<BgAgents> {
        let session = std::env::var("ZELLIJ_SESSION_NAME").ok().filter(|s| !s.is_empty())?;
        Some(BgAgents::at(&state_dir()?, &session, pane_id))
    }

    /// The markers for (`session`, `pane_id`) under `dir`. `.` is the field
    /// separator, so the session part folds it to `_`: otherwise session `a`
    /// pane 1's prefix `bg-agents.a.1.` would also match session `a.1`'s
    /// markers, and its sweep would delete them.
    pub fn at(dir: &Path, session: &str, pane_id: u32) -> BgAgents {
        let session = sanitize(session).replace('.', "_");
        BgAgents { dir: dir.to_path_buf(), prefix: format!("bg-agents.{session}.{pane_id}.") }
    }

    fn marker(&self, id: &str) -> Option<PathBuf> {
        Some(self.dir.join(format!("{}{}", self.prefix, file_id(id)?)))
    }

    /// Is `path` a marker younger than [`TTL_SECS`] as of `now`? An expired
    /// one is unlinked (best effort) and reads as absent.
    fn live(path: &Path, now: u64) -> bool {
        let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) else {
            return false;
        };
        let at = mtime.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs();
        if now.saturating_sub(at) < TTL_SECS {
            return true;
        }
        let _ = std::fs::remove_file(path);
        false
    }

    /// Unlink this pane's markers — all of them, or only the expired ones.
    fn sweep(&self, now: u64, all: bool) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name().to_str().is_some_and(|n| n.starts_with(&self.prefix)) {
                if all {
                    let _ = std::fs::remove_file(&path);
                } else {
                    BgAgents::live(&path, now);
                }
            }
        }
    }

    /// Fold this hook into the markers and say whether it is a background
    /// subagent's own hook that must send nothing. Never called on a dry run
    /// (which neither consults nor touches state, like the dedup record).
    pub fn intake(&self, raw: &str, now: u64) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            return false;
        };
        let event = str_field(&v, "hook_event_name").unwrap_or("");
        let recorded = || str_field(&v, "agent_id").and_then(|id| self.marker(id)).filter(|m| BgAgents::live(m, now));
        match event {
            "PostToolUse" => {
                let response = v.get("tool_response").unwrap_or(&Value::Null);
                if str_field(response, "status") == Some("async_launched") {
                    if let Some(marker) = str_field(response, "agentId").and_then(|id| self.marker(id)) {
                        self.sweep(now, false);
                        let _ = std::fs::write(marker, b"");
                    }
                }
                recorded().is_some()
            }
            "PreToolUse" => recorded().is_some(),
            "SubagentStop" => recorded().map(std::fs::remove_file).is_some(),
            "SessionStart" | "SessionEnd" => {
                self.sweep(now, true);
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// Payload shapes from the 2.1.282 capture.
    const LAUNCH: &str = r#"{"hook_event_name":"PostToolUse","tool_name":"Agent","tool_input":{"run_in_background":true},"tool_response":{"isAsync":true,"status":"async_launched","agentId":"a1139dfe5ebed1d64"}}"#;
    const BG_TOOL: &str = r#"{"hook_event_name":"PreToolUse","agent_id":"a1139dfe5ebed1d64","agent_type":"general-purpose","tool_name":"Read","tool_input":{"file_path":"/p/x"}}"#;
    const BG_POST: &str = r#"{"hook_event_name":"PostToolUse","agent_id":"a1139dfe5ebed1d64","agent_type":"general-purpose","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":""}}"#;
    const FG_TOOL: &str = r#"{"hook_event_name":"PostToolUse","agent_id":"a061999ea11fbeafa","agent_type":"general-purpose","tool_name":"Read","tool_input":{"file_path":"/p/x"}}"#;
    const BG_STOP: &str =
        r#"{"hook_event_name":"SubagentStop","agent_id":"a1139dfe5ebed1d64","agent_type":"general-purpose"}"#;
    fn now() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    fn tracker() -> (tempfile::TempDir, BgAgents) {
        let dir = tempfile::tempdir().unwrap();
        let t = BgAgents::at(dir.path(), "my session", 7);
        (dir, t)
    }

    fn with_id(payload: &str, id: &str) -> String {
        payload.replace("a1139dfe5ebed1d64", id)
    }

    fn markers(t: &BgAgents) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(&t.dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        names
    }

    #[test]
    fn a_launched_background_agents_hooks_are_suppressed() {
        let (_d, t) = tracker();
        assert!(!t.intake(LAUNCH, now()), "the launch itself reports");
        assert_eq!(markers(&t), vec!["bg-agents.my_session.7.a1139dfe5ebed1d64"]);
        assert!(t.intake(BG_TOOL, now()));
        assert!(t.intake(BG_POST, now()));
        // Its SubagentStop is suppressed too (a plain running over "waiting
        // on …"), and removes the marker.
        assert!(t.intake(BG_STOP, now()));
        assert!(markers(&t).is_empty());
        assert!(!t.intake(BG_TOOL, now()));
        assert!(!t.intake(BG_STOP, now()), "an unrecorded SubagentStop reports");
    }

    #[test]
    fn parallel_launches_each_keep_their_marker() {
        // Two background Agent calls in one turn: separate files, so neither
        // launch can overwrite the other's record.
        let (_d, t) = tracker();
        let (a, b) = (BgAgents::at(&t.dir, "my session", 7), BgAgents::at(&t.dir, "my session", 7));
        a.intake(&with_id(LAUNCH, "aaa"), now());
        b.intake(&with_id(LAUNCH, "bbb"), now());
        assert!(t.intake(&with_id(BG_TOOL, "aaa"), now()));
        assert!(t.intake(&with_id(BG_TOOL, "bbb"), now()));
    }

    #[test]
    fn an_unknown_agent_id_reports() {
        // A foreground subagent was never launched async: its PostToolUse is
        // the Pending-recovery edge and must go out.
        let (_d, t) = tracker();
        t.intake(LAUNCH, now());
        assert!(!t.intake(FG_TOOL, now()));
        assert!(!t.intake(r#"{"hook_event_name":"PreToolUse","tool_name":"Read"}"#, now()));
        // Markers are per pane.
        assert!(!BgAgents::at(&t.dir, "my session", 8).intake(BG_TOOL, now()));
    }

    #[test]
    fn a_missing_or_unwritable_state_dir_fails_open() {
        let gone = BgAgents::at(Path::new("/nonexistent/zj-radar"), "s", 1);
        gone.intake(LAUNCH, now());
        assert!(!gone.intake(BG_TOOL, now()));
        let (_d, t) = tracker();
        assert!(!t.intake("{not json", now()));
    }

    #[test]
    fn ids_are_filename_safe_and_capped() {
        let (_d, t) = tracker();
        t.intake(&with_id(LAUNCH, "../../etc/x"), now());
        assert_eq!(markers(&t), vec!["bg-agents.my_session.7.______etc_x"]);
        assert!(t.intake(&with_id(BG_TOOL, "../../etc/x"), now()));
        let long = "a".repeat(500);
        t.intake(&with_id(LAUNCH, &long), now());
        assert!(markers(&t)
            .iter()
            .any(|n| n.ends_with(&"a".repeat(MAX_ID_CHARS)) && !n.ends_with(&"a".repeat(MAX_ID_CHARS + 1))));
        // An id with nothing filename-safe records nothing.
        t.intake(&with_id(LAUNCH, "///"), now());
        assert!(!t.intake(&with_id(BG_TOOL, "///"), now()));
        assert_eq!(file_id("..."), None);
    }

    fn age(t: &BgAgents, id: &str, secs: u64) {
        let f = std::fs::File::options().write(true).open(t.marker(id).unwrap()).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
    }

    #[test]
    fn an_expired_marker_reads_as_absent_and_is_unlinked() {
        let (_d, t) = tracker();
        let now = now();
        t.intake(LAUNCH, now);
        age(&t, "a1139dfe5ebed1d64", TTL_SECS - 60);
        assert!(t.intake(BG_TOOL, now));
        age(&t, "a1139dfe5ebed1d64", TTL_SECS + 60);
        assert!(!t.intake(BG_TOOL, now), "expired");
        assert!(markers(&t).is_empty(), "unlinked");
        // A launch sweeps the pane's other stale markers.
        t.intake(&with_id(LAUNCH, "old"), now);
        age(&t, "old", TTL_SECS + 60);
        t.intake(&with_id(LAUNCH, "new"), now);
        assert_eq!(markers(&t), vec!["bg-agents.my_session.7.new"]);
    }

    #[test]
    fn a_session_whose_name_extends_anothers_keeps_its_markers() {
        // Session `a` pane 1 must not sweep session `a.1` pane 5's markers.
        let dir = tempfile::tempdir().unwrap();
        let (a, a1) = (BgAgents::at(dir.path(), "a", 1), BgAgents::at(dir.path(), "a.1", 5));
        a.intake(LAUNCH, now());
        a1.intake(LAUNCH, now());
        a.intake(r#"{"hook_event_name":"SessionEnd"}"#, now());
        assert!(!a.intake(BG_TOOL, now()), "a:1 cleared");
        assert!(a1.intake(BG_TOOL, now()), "a.1:5 kept its marker");
    }

    #[test]
    fn session_edges_remove_only_this_panes_markers() {
        let (_d, t) = tracker();
        let other = BgAgents::at(&t.dir, "my session", 8);
        for edge in [r#"{"hook_event_name":"SessionStart","source":"clear"}"#, r#"{"hook_event_name":"SessionEnd"}"#] {
            t.intake(LAUNCH, now());
            other.intake(LAUNCH, now());
            assert!(!t.intake(edge, now()), "{edge} reports");
            assert!(!t.intake(BG_TOOL, now()), "{edge}");
            assert!(other.intake(BG_TOOL, now()), "{edge} left pane 8 alone");
        }
    }

    #[test]
    fn unrelated_hooks_skip_everything() {
        assert!(!relevant(r#"{"hook_event_name":"PostToolUse","tool_name":"Read"}"#));
        assert!(!relevant(r#"{"hook_event_name":"PostToolUse","tool_response":{"file":"mentions SessionStart"}}"#));
        assert!(relevant(BG_TOOL) && relevant(LAUNCH) && relevant(BG_STOP));
        assert!(relevant(r#"{"hook_event_name":"SessionEnd"}"#));
    }
}
