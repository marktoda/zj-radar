//! Codex hook/notify JSON to Radar status updates.

use super::events::{tool_activity, Update};
use crate::status::Status;
use serde_json::Value;

/// Parse either modern Codex hook JSON from stdin or legacy Codex `notify`
/// argv JSON. Returns the Radar update and optional session cwd.
pub fn derive_update(raw_json: &str) -> Option<(Update, Option<String>)> {
    let v: Value = serde_json::from_str(raw_json).ok()?;
    if v.get("hook_event_name").is_some() {
        return derive_hook_update(&v);
    }
    derive_legacy_notify_update(&v)
}

fn derive_hook_update(v: &Value) -> Option<(Update, Option<String>)> {
    let event = v.get("hook_event_name")?.as_str()?;
    let cwd = string_field(v, "cwd");
    let update = match event {
        "UserPromptSubmit" => running("working"),
        "PreToolUse" | "PostToolUse" => {
            let tool_name = v.get("tool_name").and_then(|x| x.as_str()).unwrap_or("");
            let tool_input = v.get("tool_input").unwrap_or(&Value::Null);
            running(tool_activity(tool_name, tool_input).unwrap_or_else(|| "working".to_string()))
        }
        "PermissionRequest" => pending(permission_message(v)),
        "SubagentStart" => running("delegating"),
        "SubagentStop" => running(last_assistant_message(v).unwrap_or_else(|| "delegating".into())),
        "Stop" => Update {
            status: Status::Done,
            msg: last_assistant_message(v).unwrap_or_default(),
        },
        _ => return None,
    };
    Some((update, cwd))
}

fn derive_legacy_notify_update(v: &Value) -> Option<(Update, Option<String>)> {
    let ty = v.get("type")?.as_str()?;
    if ty != "agent-turn-complete" {
        return None;
    }
    Some((
        Update {
            status: Status::Done,
            msg: v
                .get("last-assistant-message")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        },
        string_field(v, "cwd"),
    ))
}

fn running(msg: impl Into<String>) -> Update {
    Update {
        status: Status::Running,
        msg: msg.into(),
    }
}

fn pending(msg: impl Into<String>) -> Update {
    Update {
        status: Status::Pending,
        msg: msg.into(),
    }
}

fn permission_message(v: &Value) -> String {
    v.pointer("/tool_input/description")
        .and_then(|x| x.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| v.get("message").and_then(|x| x.as_str()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("approval requested")
        .to_string()
}

fn last_assistant_message(v: &Value) -> Option<String> {
    v.get("last_assistant_message")
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

fn string_field(v: &Value, field: &str) -> Option<String> {
    v.get(field)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(raw: &str) -> (Update, Option<String>) {
        derive_update(raw).unwrap()
    }

    #[test]
    fn user_prompt_submit_is_running() {
        let (u, cwd) = update(
            r#"{
              "hook_event_name": "UserPromptSubmit",
              "cwd": "/repo",
              "prompt": "fix it"
            }"#,
        );
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
        assert_eq!(cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn tool_hooks_use_tool_activity() {
        let (u, _) = update(
            r#"{
              "hook_event_name": "PreToolUse",
              "tool_name": "Bash",
              "tool_input": { "command": "cargo test --features cli" }
            }"#,
        );
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "running tests");

        let (u, _) = update(
            r#"{
              "hook_event_name": "PostToolUse",
              "tool_name": "apply_patch",
              "tool_input": { "command": "apply_patch <<'PATCH'" }
            }"#,
        );
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "editing files");
    }

    #[test]
    fn permission_request_is_pending_with_description() {
        let (u, _) = update(
            r#"{
              "hook_event_name": "PermissionRequest",
              "tool_name": "Bash",
              "tool_input": {
                "command": "git push",
                "description": "Approve network access?"
              }
            }"#,
        );
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "Approve network access?");
    }

    #[test]
    fn permission_request_has_generic_fallback() {
        let (u, _) = update(
            r#"{
              "hook_event_name": "PermissionRequest",
              "tool_name": "Bash",
              "tool_input": { "command": "git push" }
            }"#,
        );
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "approval requested");
    }

    #[test]
    fn subagent_events_stay_running() {
        let (u, _) = update(r#"{"hook_event_name": "SubagentStart"}"#);
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "delegating");

        let (u, _) = update(
            r#"{
              "hook_event_name": "SubagentStop",
              "last_assistant_message": "reviewed the tests"
            }"#,
        );
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "reviewed the tests");
    }

    #[test]
    fn stop_is_done_with_last_message() {
        let (u, _) = update(
            r#"{
              "hook_event_name": "Stop",
              "last_assistant_message": "implemented"
            }"#,
        );
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "implemented");
    }

    #[test]
    fn legacy_notify_turn_complete_still_works() {
        let (u, cwd) = update(
            r#"{
              "type": "agent-turn-complete",
              "last-assistant-message": "shipped it",
              "cwd": "/repo"
            }"#,
        );
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "shipped it");
        assert_eq!(cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn unknown_or_bad_events_are_noops() {
        assert!(derive_update("not json").is_none());
        assert!(derive_update(r#"{"hook_event_name":"SessionStart"}"#).is_none());
        assert!(derive_update(r#"{"type":"task-started"}"#).is_none());
    }
}
