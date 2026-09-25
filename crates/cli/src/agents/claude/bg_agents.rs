//! Background subagents launched from a pane, so their own tool hooks can be
//! dropped before they clobber the parent's row.
//!
//! A subagent's tool calls fire Pre/PostToolUse on the parent's pane, stamped
//! with its `agent_id` (verified live on Claude Code 2.1.282; the parent's
//! own hooks never carry one). For a *background* subagent they arrive after
//! the parent's turn ended, so as plain running payloads they overwrote
//! "waiting on …" and downgraded a `needs you` to running before the user
//! answered. A *foreground* subagent's hooks must keep reporting: its
//! PostToolUse is the Pending-recovery edge after a permission answered
//! inside it. The hooks themselves can't tell the two apart, but the launch
//! can: the parent's `Agent` PostToolUse answers a background launch with
//! `{status: "async_launched", agentId}`, and that `agentId` is the same
//! string as the subagent's hook `agent_id` (verified live). So this module
//! remembers those ids per (session, pane) and drops Pre/PostToolUse whose
//! `agent_id` is one of them.
//!
//! The record is small and self-healing: at most [`MAX_IDS`] ids, each
//! expiring after [`TTL_SECS`]; an id leaves on its `SubagentStop`, and the
//! pane's `SessionStart`/`SessionEnd` (the `idle` edges) clear it. Any IO or
//! parse failure fails open — the hook reports, as it did before this
//! existed. CLI-only: notify.sh's bash fallback keeps no state and reports
//! every subagent hook (parity.bats documents the divergence).

use crate::dedup::{sanitize, state_dir};
use crate::fsutil::atomic_write;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Most background subagents one pane is tracked for; the oldest drops first.
pub const MAX_IDS: usize = 16;
/// A launch older than this is forgotten even without its `SubagentStop`
/// (a crashed session, a missed hook): no subagent runs for hours, and a
/// stale id can only suppress hooks carrying that exact random id.
pub const TTL_SECS: u64 = 6 * 60 * 60;

/// The state file: `(agent id, launch epoch seconds)`, oldest first.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
struct Launched {
    ids: Vec<(String, u64)>,
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The pure rule: fold one hook payload into `rec` as of `now`, returning
/// `(changed, suppress)`.
fn step(rec: &mut Launched, v: &Value, now: u64) -> (bool, bool) {
    let before = rec.ids.len();
    rec.ids.retain(|(_, at)| now.saturating_sub(*at) < TTL_SECS);
    let mut changed = rec.ids.len() != before;
    let event = str_field(v, "hook_event_name").unwrap_or("");
    let agent_id = str_field(v, "agent_id");
    match event {
        "PostToolUse" => {
            let response = v.get("tool_response").unwrap_or(&Value::Null);
            if str_field(response, "status") == Some("async_launched") {
                if let Some(id) = str_field(response, "agentId") {
                    rec.ids.retain(|(known, _)| known != id);
                    rec.ids.push((id.to_string(), now));
                    let excess = rec.ids.len().saturating_sub(MAX_IDS);
                    rec.ids.drain(..excess);
                    changed = true;
                }
            }
        }
        "SubagentStop" => {
            if let Some(id) = agent_id {
                let n = rec.ids.len();
                rec.ids.retain(|(known, _)| known != id);
                changed |= rec.ids.len() != n;
            }
        }
        "SessionStart" | "SessionEnd" => {
            changed |= !rec.ids.is_empty();
            rec.ids.clear();
        }
        _ => {}
    }
    let suppress = matches!(event, "PreToolUse" | "PostToolUse")
        && agent_id.is_some_and(|id| rec.ids.iter().any(|(known, _)| known == id));
    (changed, suppress)
}

/// Cheap pre-filter on the raw payload: only these hooks can touch the
/// record or be suppressed, so every other hook (the common case: the
/// parent's own tool calls) skips the JSON parse and the file read.
fn relevant(raw: &str) -> bool {
    ["\"agent_id\"", "async_launched", "SessionStart", "SessionEnd"].iter().any(|n| raw.contains(n))
}

/// One pane's record of background subagents.
pub(crate) struct BgAgents {
    path: PathBuf,
}

impl BgAgents {
    /// The record for `pane_id` in the current Zellij session, or `None`
    /// without a session identity to scope it by (nothing is suppressed).
    pub fn from_env(pane_id: u32) -> Option<BgAgents> {
        let session = std::env::var("ZELLIJ_SESSION_NAME").ok().filter(|s| !s.is_empty())?;
        Some(BgAgents::at(&state_dir(), &session, pane_id))
    }

    /// The record file for (`session`, `pane_id`) under `dir`.
    pub fn at(dir: &Path, session: &str, pane_id: u32) -> BgAgents {
        BgAgents { path: dir.join(format!("bg-agents.{}.{pane_id}.json", sanitize(session))) }
    }

    /// Fold this hook into the record (unless `dry_run`) and say whether it
    /// is a background subagent's own tool hook that must send nothing. A
    /// missing or malformed file reads as empty: report.
    pub fn intake(&self, raw: &str, now: u64, dry_run: bool) -> bool {
        if !relevant(raw) {
            return false;
        }
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            return false;
        };
        let mut rec: Launched = std::fs::read(&self.path)
            .ok()
            .and_then(|body| serde_json::from_slice(&body).ok())
            .unwrap_or_default();
        let (changed, suppress) = step(&mut rec, &v, now);
        if changed && !dry_run {
            if let Ok(body) = serde_json::to_vec(&rec) {
                let _ = atomic_write(&self.path, &body);
            }
        }
        suppress
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Payload shapes from the 2.1.282 capture.
    const LAUNCH: &str = r#"{"hook_event_name":"PostToolUse","tool_name":"Agent","tool_input":{"run_in_background":true},"tool_response":{"isAsync":true,"status":"async_launched","agentId":"a1139dfe5ebed1d64"}}"#;
    const BG_TOOL: &str = r#"{"hook_event_name":"PreToolUse","agent_id":"a1139dfe5ebed1d64","agent_type":"general-purpose","tool_name":"Read","tool_input":{"file_path":"/p/x"}}"#;
    const BG_POST: &str = r#"{"hook_event_name":"PostToolUse","agent_id":"a1139dfe5ebed1d64","agent_type":"general-purpose","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":""}}"#;
    const FG_TOOL: &str = r#"{"hook_event_name":"PostToolUse","agent_id":"a061999ea11fbeafa","agent_type":"general-purpose","tool_name":"Read","tool_input":{"file_path":"/p/x"}}"#;
    const BG_STOP: &str = r#"{"hook_event_name":"SubagentStop","agent_id":"a1139dfe5ebed1d64","agent_type":"general-purpose"}"#;

    fn tracker() -> (tempfile::TempDir, BgAgents) {
        let dir = tempfile::tempdir().unwrap();
        let t = BgAgents::at(dir.path(), "my session", 7);
        (dir, t)
    }

    #[test]
    fn a_launched_background_agents_tool_hooks_are_suppressed() {
        let (_d, t) = tracker();
        assert!(!t.intake(LAUNCH, 100, false), "the launch itself reports");
        assert!(t.intake(BG_TOOL, 101, false));
        assert!(t.intake(BG_POST, 102, false));
        // SubagentStop reports, and forgets the id.
        assert!(!t.intake(BG_STOP, 103, false));
        assert!(!t.intake(BG_TOOL, 104, false));
    }

    #[test]
    fn an_unknown_agent_id_reports() {
        // A foreground subagent was never launched async: its PostToolUse is
        // the Pending-recovery edge and must go out.
        let (_d, t) = tracker();
        t.intake(LAUNCH, 100, false);
        assert!(!t.intake(FG_TOOL, 101, false));
        // The parent's own hooks carry no agent_id.
        assert!(!t.intake(r#"{"hook_event_name":"PreToolUse","tool_name":"Read"}"#, 101, false));
    }

    #[test]
    fn an_absent_or_corrupt_record_fails_open() {
        let (_d, t) = tracker();
        assert!(!t.intake(BG_TOOL, 100, false), "no record");
        std::fs::write(&t.path, b"{not json").unwrap();
        assert!(!t.intake(BG_TOOL, 100, false), "corrupt record");
        // …and a fresh launch overwrites the junk.
        t.intake(LAUNCH, 100, false);
        assert!(t.intake(BG_TOOL, 101, false));
        // An unwritable location reports too.
        let gone = BgAgents::at(Path::new("/nonexistent/zj-radar"), "s", 1);
        gone.intake(LAUNCH, 100, false);
        assert!(!gone.intake(BG_TOOL, 101, false));
    }

    #[test]
    fn ids_expire_and_session_edges_clear_them() {
        let (_d, t) = tracker();
        t.intake(LAUNCH, 100, false);
        assert!(t.intake(BG_TOOL, 100 + TTL_SECS - 1, false));
        assert!(!t.intake(BG_TOOL, 100 + TTL_SECS, false), "expired");
        for edge in [r#"{"hook_event_name":"SessionStart","source":"clear"}"#, r#"{"hook_event_name":"SessionEnd"}"#] {
            t.intake(LAUNCH, 200, false);
            t.intake(edge, 201, false);
            assert!(!t.intake(BG_TOOL, 202, false), "{edge}");
        }
    }

    #[test]
    fn the_record_is_bounded_oldest_first() {
        let (_d, t) = tracker();
        for i in 0..=MAX_IDS {
            let launch = LAUNCH.replace("a1139dfe5ebed1d64", &format!("id{i}"));
            t.intake(&launch, 100 + i as u64, false);
        }
        let tool = |id: &str| BG_TOOL.replace("a1139dfe5ebed1d64", id);
        assert!(!t.intake(&tool("id0"), 200, false), "the oldest was dropped");
        assert!(t.intake(&tool("id1"), 200, false));
        assert!(t.intake(&tool(&format!("id{MAX_IDS}")), 200, false));
    }

    #[test]
    fn a_dry_run_reads_but_never_writes() {
        let (_d, t) = tracker();
        t.intake(LAUNCH, 100, true);
        assert!(!t.path.exists());
        t.intake(LAUNCH, 100, false);
        assert!(t.intake(BG_TOOL, 101, true));
    }

    #[test]
    fn unrelated_hooks_skip_the_parse() {
        assert!(!relevant(r#"{"hook_event_name":"PostToolUse","tool_name":"Read"}"#));
        assert!(relevant(BG_TOOL) && relevant(LAUNCH) && relevant(BG_STOP));
    }
}
