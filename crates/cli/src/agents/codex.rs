//! Codex hook/notify JSON → Radar status update.

use super::{tool_activity, AgentUpdate, Intake};
use crate::status::Status;
use serde_json::Value;

/// Parse either modern Codex hook JSON or legacy Codex `notify` argv JSON.
/// Codex ignores `status_arg` (it has no explicit-status override).
pub fn derive(intake: &Intake) -> Option<AgentUpdate> {
    let v: Value = serde_json::from_str(intake.raw).ok()?;
    if v.get("hook_event_name").is_some() {
        return derive_hook_update(&v);
    }
    derive_legacy_notify_update(&v)
}

fn derive_hook_update(v: &Value) -> Option<AgentUpdate> {
    let event = v.get("hook_event_name")?.as_str()?;
    let cwd = string_field(v, "cwd");
    let (status, msg) = match event {
        "UserPromptSubmit" => (Status::Running, "working".to_string()),
        "PreToolUse" | "PostToolUse" => {
            let tool_name = v.get("tool_name").and_then(|x| x.as_str()).unwrap_or("");
            let tool_input = v.get("tool_input").unwrap_or(&Value::Null);
            (
                Status::Running,
                tool_activity(tool_name, tool_input).unwrap_or_else(|| "working".to_string()),
            )
        }
        "PermissionRequest" => (Status::Pending, permission_message(v)),
        "SubagentStart" => (Status::Running, "delegating".to_string()),
        "SubagentStop" => (
            Status::Running,
            last_assistant_message(v).unwrap_or_else(|| "delegating".into()),
        ),
        "Stop" => (Status::Done, last_assistant_message(v).unwrap_or_default()),
        _ => return None,
    };
    let task = if event == "UserPromptSubmit" {
        v.get("prompt")
            .and_then(|x| x.as_str())
            .and_then(super::task_from_prompt)
    } else {
        None
    };
    Some(AgentUpdate { status, msg, cwd, task })
}

fn derive_legacy_notify_update(v: &Value) -> Option<AgentUpdate> {
    let ty = v.get("type")?.as_str()?;
    if ty != "agent-turn-complete" {
        return None;
    }
    Some(AgentUpdate {
        status: Status::Done,
        msg: v
            .get("last-assistant-message")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        cwd: string_field(v, "cwd"),
        task: None,
    })
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

    fn update(raw: &str) -> AgentUpdate {
        derive(&Intake {
            raw,
            status_arg: None,
        })
        .unwrap()
    }

    #[test]
    fn user_prompt_submit_is_running() {
        let u = update(
            r#"{
              "hook_event_name": "UserPromptSubmit",
              "cwd": "/repo",
              "prompt": "fix it"
            }"#,
        );
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
        assert_eq!(u.cwd.as_deref(), Some("/repo"));
        assert_eq!(u.task.as_deref(), Some("fix it"));
    }

    #[test]
    fn only_prompt_submit_carries_a_task() {
        let u = update(r#"{"hook_event_name":"Stop","last_assistant_message":"implemented"}"#);
        assert_eq!(u.task, None);
    }

    #[test]
    fn tool_hooks_use_tool_activity() {
        let u = update(
            r#"{
              "hook_event_name": "PreToolUse",
              "tool_name": "Bash",
              "tool_input": { "command": "cargo test --features cli" }
            }"#,
        );
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "running tests");

        let u = update(
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
        let u = update(
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
        let u = update(
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
        let u = update(r#"{"hook_event_name": "SubagentStart"}"#);
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "delegating");

        let u = update(
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
        let u = update(
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
        let u = update(
            r#"{
              "type": "agent-turn-complete",
              "last-assistant-message": "shipped it",
              "cwd": "/repo"
            }"#,
        );
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "shipped it");
        assert_eq!(u.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn unknown_or_bad_events_are_noops() {
        let none = |raw: &str| {
            derive(&Intake {
                raw,
                status_arg: None,
            })
        };
        assert!(none("not json").is_none());
        assert!(none(r#"{"hook_event_name":"SessionStart"}"#).is_none());
        assert!(none(r#"{"type":"task-started"}"#).is_none());
    }

    #[test]
    fn status_arg_is_ignored_by_codex() {
        // Codex derives purely from the payload; an explicit status arg is a no-op.
        let u = derive(&Intake {
            raw: r#"{"hook_event_name":"UserPromptSubmit"}"#,
            status_arg: Some("done"),
        })
        .unwrap();
        assert_eq!(u.status, Status::Running);
    }
}
