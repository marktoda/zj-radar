//! GitHub Copilot CLI hook payload → Radar status update.
//!
//! Only lifecycle events with no documented decision/response contract are
//! wired: `sessionStart`, `sessionEnd`, `userPromptSubmitted`, `agentStop`,
//! `errorOccurred`. GitHub's Copilot hooks reference confirms `preToolUse`
//! and `permissionRequest` are fail-closed (a crash, a missing `zj-radar` on
//! `PATH`, or any non-timeout non-zero exit denies the tool call) — a
//! status producer must never be able to break the agent it reports on, so
//! neither is wired, and `postToolUse` is left out too since its own
//! safety was never proven. `subagentStart`/`subagentStop` are replaced by
//! the blanket `agentId`/`parentAgentId` filter below, which also covers a
//! background tool call's own events, not just the two dedicated ones.
//! Consequence: **approvals and tool activity are not observed at all**
//! for Copilot panes.
//!
//! Copilot's hook payload carries no field naming which event fired, so the
//! hook file gives every wired event its own entry passing an explicit
//! `--status`. No raw user content crosses the wire: `userPromptSubmitted`'s
//! `prompt` is never read (no task label, unlike the other adapters —
//! deliberate, since the wire payload is a cross-session surface);
//! `agentStop`'s real payload has no message-shaped field at all
//! (`sessionId`/`timestamp`/`cwd`/`transcriptPath`/`stopReason`/
//! `stop_hook_active`) and `transcriptPath` (the full conversation) is
//! never opened; `errorOccurred`'s top-level `recoverable` (a sibling of
//! `error`, not nested under it) decides whether to report — `true` is
//! Copilot's own normal retry/continue, not a failure — and `error.message`
//! is never read at all, not even to decide.
//!
//! Coverage and limitations: `docs/producers.md#copilot-cli`.

use super::{baseline_msg, string_field, AgentUpdate, Intake};
use crate::status::Status;
use serde_json::Value;

/// Field names that mark an event as scoped to a subagent or other
/// background context, in both Copilot's native camelCase and the
/// Claude-Code-style snake_case some events may carry instead — Copilot's
/// own payloads aren't uniformly cased (its real `agentStop` carries both
/// `stopReason` and `stop_hook_active`).
const SUBAGENT_FIELDS: [&str; 4] = ["agentId", "agent_id", "parentAgentId", "parent_agent_id"];

pub fn derive(intake: &Intake) -> Option<AgentUpdate> {
    let status = Status::from_wire(intake.status_arg?);
    let v: Value = serde_json::from_str(intake.raw).unwrap_or(Value::Null);
    // A background/subagent-scoped event must never clobber the foreground
    // pane's row — it can arrive late, out of order with the main turn's
    // own hooks. Checked before anything else derives from the payload.
    if is_subagent_scoped(&v) {
        return None;
    }
    let cwd = string_field(&v, "cwd");

    match status {
        // userPromptSubmitted: the only event wired to `running`. No field
        // read beyond `cwd`: the prompt's text is deliberately never
        // captured as a task label.
        Status::Running => Some(AgentUpdate { status, msg: baseline_msg(status, ""), cwd, task: None, tasks: None }),
        // agentStop: the foreground turn ending. The real payload has no
        // message-shaped field, so this reports a bare "done" rather than
        // fabricate one — and never reads `transcriptPath`.
        Status::Done => Some(AgentUpdate { status, msg: String::new(), cwd, task: None, tasks: None }),
        // errorOccurred: the top-level `recoverable` field (a sibling of
        // `error`, never nested under it) decides whether to report at all
        // — a recoverable error is Copilot's own normal retry/continue
        // behavior, not a failure. `error.message` is never read, for any
        // purpose: the model's raw error text could carry file content or
        // secrets, so a real failure always gets the fixed generic label.
        Status::Error => {
            let recoverable = v.get("recoverable").and_then(Value::as_bool).unwrap_or(false);
            if recoverable {
                return None;
            }
            Some(AgentUpdate { status, msg: "errored".to_string(), cwd, task: None, tasks: None })
        }
        // sessionStart / sessionEnd: reset the row unconditionally. Safe
        // either way: a brand-new `copilot` process has no in-flight turn
        // to clobber, and a stale row from one that already exited is also
        // cleared by the pane returning to its shell prompt.
        Status::Idle => Some(AgentUpdate { status, msg: String::new(), cwd, task: None, tasks: None }),
        Status::Pending => None, // never wired: permissionRequest is unsafe (see module docs)
    }
}

/// True when the payload carries a non-empty subagent/parent-agent id under
/// either casing Copilot uses. See [`SUBAGENT_FIELDS`].
fn is_subagent_scoped(v: &Value) -> bool {
    SUBAGENT_FIELDS.iter().any(|key| v.get(*key).and_then(Value::as_str).is_some_and(|s| !s.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(status: &str, raw: &str) -> Option<AgentUpdate> {
        derive(&Intake { raw, status_arg: Some(status) })
    }

    #[test]
    fn user_prompt_submitted_is_running_with_no_task_and_no_prompt_read() {
        // Even a prompt field present on the payload must never surface as
        // a task label — the whole point of not reading it.
        let u = update("running", r#"{"cwd": "/repo", "prompt": "fix the flaky test and leak nothing"}"#).unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
        assert_eq!(u.cwd.as_deref(), Some("/repo"));
        assert_eq!(u.task, None, "userPromptSubmitted must never capture a task label for Copilot");
    }

    #[test]
    fn agent_stop_is_a_bare_done_never_a_fabricated_message() {
        // The real agentStop payload: sessionId/timestamp/cwd/transcriptPath/
        // stopReason/stop_hook_active — no message-shaped field at all.
        let u = update(
            "done",
            r#"{"sessionId":"s1","timestamp":123,"cwd":"/repo","transcriptPath":"/home/u/.copilot/transcripts/s1.jsonl","stopReason":"end_turn","stop_hook_active":false}"#,
        )
        .unwrap();
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "", "no field in the real payload is a safe source for turn-end text");
        assert_eq!(u.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn agent_stop_never_reads_an_invented_message_field() {
        // Even if some future build adds a `message`-shaped field, this
        // adapter must not start reading it without re-auditing the privacy
        // story — pin the current, deliberate behavior.
        let u = update("done", r#"{"message": "Refactored.\n\nShould I push?"}"#).unwrap();
        assert_eq!(u.status, Status::Done, "no trailing-question remap: there is no real field to remap from");
        assert_eq!(u.msg, "");
    }

    #[test]
    fn error_occurred_recoverable_is_ignored() {
        // Real (canonical) shape: `recoverable` is top-level, a sibling of
        // `error`, never nested under it.
        assert!(
            update("error", r#"{"error": {"message": "rate limited, retrying"}, "recoverable": true}"#).is_none(),
            "a recoverable error is Copilot's own normal retry/continue behavior, not a reportable failure"
        );
    }

    #[test]
    fn error_occurred_non_recoverable_is_error_with_a_generic_label_never_the_message() {
        let u = update(
            "error",
            r#"{"error": {"message": "contains a secret the rail must never show"}, "recoverable": false}"#,
        )
        .unwrap();
        assert_eq!(u.status, Status::Error);
        assert_eq!(u.msg, "errored");
        assert!(!u.msg.contains("secret"));
    }

    #[test]
    fn error_occurred_missing_recoverable_defaults_to_reporting() {
        // Absent is not the same as recoverable:true — default to still
        // being a useful producer rather than silently swallowing it.
        let u = update("error", r#"{"error": {"message": "boom"}}"#).unwrap();
        assert_eq!(u.status, Status::Error);
        assert_eq!(u.msg, "errored");
    }

    #[test]
    fn error_occurred_ignores_a_nested_recoverable_look_alike() {
        // A `recoverable` nested under `error` (the old, wrong shape this
        // adapter used to read) must NOT be mistaken for the real top-level
        // field — pins the fix against regressing back to the wrong path.
        let u = update("error", r#"{"error": {"message": "boom", "recoverable": true}}"#).unwrap();
        assert_eq!(u.status, Status::Error, "a nested recoverable is not the real field and must not suppress this");
        assert_eq!(u.msg, "errored");
    }

    #[test]
    fn session_start_and_session_end_are_idle() {
        for raw in [r#"{"cwd": "/repo"}"#, r#"{}"#] {
            let u = update("idle", raw).unwrap();
            assert_eq!(u.status, Status::Idle);
            assert_eq!(u.msg, "");
            assert_eq!(u.task, None);
        }
    }

    #[test]
    fn subagent_scoped_events_are_dropped_under_either_field_casing() {
        for raw in [
            r#"{"cwd": "/repo", "agentId": "sub-1"}"#,
            r#"{"cwd": "/repo", "parentAgentId": "main-1"}"#,
            r#"{"cwd": "/repo", "agent_id": "sub-1"}"#,
            r#"{"cwd": "/repo", "parent_agent_id": "main-1"}"#,
        ] {
            assert!(update("running", raw).is_none(), "{raw}");
            assert!(update("done", raw).is_none(), "{raw}");
        }
        // An empty id is not a real subagent scope.
        assert!(update("running", r#"{"agentId": ""}"#).is_some());
    }

    #[test]
    fn permission_request_is_never_wired() {
        // There is no `--status pending` path left: `setup copilot` no
        // longer registers `permissionRequest` at all, and a hand-crafted
        // `--status pending` (e.g. from version skew) must still no-op
        // rather than show a signal we can no longer stand behind.
        assert!(update("pending", r#"{"toolName": "Bash"}"#).is_none());
    }

    #[test]
    fn missing_status_arg_is_a_no_op() {
        // `setup copilot`'s hook file always supplies `--status`; a direct
        // invocation without one (hand-testing, version skew) must no-op
        // rather than guess.
        assert!(derive(&Intake { raw: r#"{"prompt": "hi"}"#, status_arg: None }).is_none());
    }

    /// Weld: every status token `setup copilot`'s hook file can pass must
    /// derive a real update from a minimal, plausible payload — an exhaustive
    /// `match status` already makes a silently-unhandled token a compile
    /// error, but this additionally guards the runtime behavior end to end.
    #[test]
    fn every_wired_status_token_derives_an_update() {
        for (status, raw) in [
            ("running", r#"{"prompt": "hi"}"#),
            ("done", r#"{"stopReason": "end_turn"}"#),
            ("error", r#"{"error": {"message": "boom"}, "recoverable": false}"#),
            ("idle", r#"{}"#),
        ] {
            assert!(
                derive(&Intake { raw, status_arg: Some(status) }).is_some(),
                "`setup copilot` wires a {status} hook, but derive no-ops on it"
            );
        }
    }
}
