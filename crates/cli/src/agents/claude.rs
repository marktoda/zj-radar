//! Claude Code hook payload → Radar status update.

use super::{tool_activity, AgentUpdate, Intake};
use crate::status::Status;
use serde_json::Value;

const GENERIC_PENDING: [&str; 2] = ["Claude needs attention", "Claude Code needs your attention"];

/// Map a Claude hook event name to a status (used when `--status` is absent).
fn status_from_event(event: &str) -> Option<Status> {
    match event {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "SubagentStop" => Some(Status::Running),
        "Notification" => Some(Status::Pending),
        "Stop" => Some(Status::Done),
        _ => None,
    }
}

/// Decide Claude's status + msg + cwd. `status_arg` (from the matcher-driven
/// hooks.json) wins; else derive from `hook_event_name`. Applies the pending
/// backstop, the running-with-no-activity baseline, and — for Pre/PostToolUse —
/// substitutes the live tool-activity string. Returns `None` for a no-op.
pub fn derive(intake: &Intake) -> Option<AgentUpdate> {
    let v: Value = serde_json::from_str(intake.raw).unwrap_or(Value::Null);
    let event = v.get("hook_event_name").and_then(|x| x.as_str());
    let msg = v
        .get("message")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("last_assistant_message").and_then(|x| x.as_str()))
        .unwrap_or("");

    let status = match intake.status_arg {
        Some(s) => Status::from_wire(s),
        None => status_from_event(event?)?,
    };

    if status == Status::Pending {
        let m = msg.trim();
        if m.is_empty() || GENERIC_PENDING.contains(&m) {
            return None; // backstop: not a real "needs you"
        }
    }

    // A running broadcast with no message renders as a blank active row. Give it
    // a neutral baseline; the tool-activity substitution below refines it when a
    // tool name/input is present.
    let mut out_msg = if status == Status::Running && msg.trim().is_empty() {
        "working".to_string()
    } else {
        msg.to_string()
    };

    // For PreToolUse/PostToolUse, show the live action instead of the baseline.
    if status == Status::Running && matches!(event, Some("PreToolUse") | Some("PostToolUse")) {
        let tool_name = v.get("tool_name").and_then(|x| x.as_str()).unwrap_or("");
        let tool_input = v.get("tool_input").unwrap_or(&Value::Null);
        if let Some(activity) = tool_activity(tool_name, tool_input) {
            out_msg = activity;
        }
    }

    let cwd = v.get("cwd").and_then(|x| x.as_str()).map(str::to_string);
    Some(AgentUpdate {
        status,
        msg: out_msg,
        cwd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an intake from raw JSON and an optional explicit status.
    fn intake<'a>(raw: &'a str, status_arg: Option<&'a str>) -> Intake<'a> {
        Intake { raw, status_arg }
    }

    #[test]
    fn explicit_status_passes_through() {
        let u = derive(&intake(r#"{"message":"anything"}"#, Some("running"))).unwrap();
        assert_eq!(u.status, Status::Running);
    }

    #[test]
    fn pending_with_real_message_is_kept() {
        let u = derive(&intake(r#"{"message":"approve this?"}"#, Some("pending"))).unwrap();
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "approve this?");
    }

    #[test]
    fn pending_backstop_drops_empty_and_generic() {
        assert!(derive(&intake(r#"{"message":""}"#, Some("pending"))).is_none());
        assert!(derive(&intake(r#"{"message":"Claude needs attention"}"#, Some("pending"))).is_none());
        assert!(
            derive(&intake(
                r#"{"message":"Claude Code needs your attention"}"#,
                Some("pending")
            ))
            .is_none()
        );
    }

    #[test]
    fn running_with_empty_msg_falls_back_to_working() {
        // A running broadcast with no activity must not render as a blank active
        // row — derive a neutral "working" baseline.
        let u = derive(&intake(r#"{}"#, Some("running"))).unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
        // Whitespace-only is also empty.
        assert_eq!(
            derive(&intake(r#"{"message":"   "}"#, Some("running")))
                .unwrap()
                .msg,
            "working"
        );
        // Event-derived running (no explicit status) with no message too.
        assert_eq!(
            derive(&intake(r#"{"hook_event_name":"UserPromptSubmit"}"#, None))
                .unwrap()
                .msg,
            "working"
        );
    }

    #[test]
    fn running_with_real_msg_is_unchanged() {
        let u = derive(&intake(r#"{"message":"compiling"}"#, Some("running"))).unwrap();
        assert_eq!(u.msg, "compiling");
    }

    #[test]
    fn derives_status_from_event_when_no_explicit_status() {
        assert_eq!(
            derive(&intake(r#"{"hook_event_name":"UserPromptSubmit"}"#, None))
                .unwrap()
                .status,
            Status::Running
        );
        assert_eq!(
            derive(&intake(r#"{"hook_event_name":"PostToolUse"}"#, None))
                .unwrap()
                .status,
            Status::Running
        );
        assert_eq!(
            derive(&intake(r#"{"hook_event_name":"Stop","message":"done"}"#, None))
                .unwrap()
                .status,
            Status::Done
        );
        assert!(derive(&intake(r#"{"hook_event_name":"SomethingElse"}"#, None)).is_none());
    }

    #[test]
    fn cwd_is_extracted_from_payload() {
        let u = derive(&intake(
            r#"{"hook_event_name":"Stop","message":"done","cwd":"/home/u/repo"}"#,
            None,
        ))
        .unwrap();
        assert_eq!(u.cwd.as_deref(), Some("/home/u/repo"));
        // Absent cwd is None (run() applies the fallback).
        let u2 = derive(&intake(r#"{"hook_event_name":"Stop"}"#, None)).unwrap();
        assert_eq!(u2.cwd, None);
    }

    /// Tool-activity substitution now lives behind the seam (it was previously
    /// stranded in `notify::run`, reachable only through the full IO path).
    #[test]
    fn pretooluse_substitutes_tool_activity() {
        let u = derive(&intake(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"/p/auth.rs"}}"#,
            None,
        ))
        .unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "editing auth.rs");

        let bash = derive(&intake(
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#,
            None,
        ))
        .unwrap();
        assert_eq!(bash.msg, "pushing");
    }

    #[test]
    fn tool_activity_only_applies_to_running_tool_events() {
        // Stop is Done, not running → no tool-activity substitution even if a
        // tool_name is somehow present; the message wins.
        let u = derive(&intake(
            r#"{"hook_event_name":"Stop","message":"shipped","tool_name":"Edit","tool_input":{"file_path":"/p/x.rs"}}"#,
            None,
        ))
        .unwrap();
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "shipped");
    }
}
