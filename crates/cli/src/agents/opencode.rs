//! Opencode plugin bridge → Radar status update.
//!
//! The opencode producer is a thin JS bridge (see
//! `setup/opencode_plugin.js`) that serializes each hook/bus-event payload and
//! spawns `zj-radar notify opencode --status <s>` with JSON on stdin. The
//! bridge picks the status class (it knows the event); this adapter owns the
//! refinements keyed off the payload's `event` field: the pending backstop,
//! the running baseline, tool-activity substitution (opencode tool names/args
//! normalized into the shared `tool_activity` vocabulary), the sticky task
//! capture, and the trailing-question Done→Pending remap. Returns `None` for a
//! no-op. `session.error` maps to `Status::Error` — a real failure signal
//! Claude's hook model deliberately lacks (see the claude.rs header comment).

use super::{string_field, tool_activity, AgentUpdate, Intake};
use crate::status::Status;
use serde_json::Value;

/// Map an opencode event name to a status, used when `--status` is absent (the
/// bridge always passes `--status`, so this is the robustness/test path).
fn status_from_event(event: &str) -> Option<Status> {
    match event {
        "chat.message" | "tool.execute" | "permission.replied" => Some(Status::Running),
        "permission.ask" => Some(Status::Pending),
        "session.idle" => Some(Status::Done),
        "session.error" => Some(Status::Error),
        "session.lifecycle" => Some(Status::Idle),
        _ => None,
    }
}

/// Decide opencode's status + msg + cwd. `status_arg` (the bridge always
/// passes it) wins; else derive from the `event` field. Applies the pending
/// backstop, the running baseline, tool-activity substitution for tool events,
/// the task capture for chat.message, and the trailing-question remap for a
/// Done that ends by asking. Returns `None` for a no-op.
pub fn derive(intake: &Intake) -> Option<AgentUpdate> {
    let v: Value = serde_json::from_str(intake.raw).unwrap_or(Value::Null);
    let event = v.get("event").and_then(|x| x.as_str()).unwrap_or("");
    // `message` carries the event's text (a permission title, the tracked last
    // assistant text on idle, an error message). The user's submitted prompt
    // (`prompt`) is task-capture-only below — it must NOT become the running
    // msg, or the row would show the prompt instead of the "working" baseline.
    let msg = v.get("message").and_then(|x| x.as_str()).unwrap_or("");
    let cwd = string_field(&v, "cwd");

    let status = match intake.status_arg {
        Some(s) => Status::from_wire(s),
        None => status_from_event(event)?,
    };

    // A turn that ends by asking the user something is blocked on input, not
    // finished — opencode's `session.idle` carries no terminal-outcome flag,
    // so a prose question just surfaces as idle. Remap that Done to Pending
    // with the trailing question as the message (same rule as the Claude and
    // Codex adapters' Stop).
    if status == Status::Done {
        if let Some(question) = super::trailing_question(msg) {
            return Some(AgentUpdate {
                status: Status::Pending,
                msg: question.to_string(),
                cwd,
                task: None,
            });
        }
    }

    // Pending backstop: a permission whose title is blank is not a real
    // "needs you" — drop it rather than paint a generic pending row.
    if status == Status::Pending && msg.trim().is_empty() {
        return None;
    }

    let mut out_msg = super::baseline_msg(status, msg);

    // An error event with no message still reads as an error via its color/mark,
    // but a neutral label is friendlier than a blank red row.
    if status == Status::Error && out_msg.trim().is_empty() {
        out_msg = "errored".to_string();
    }

    // For tool events, show the live action instead of the baseline. opencode
    // tool names are lowercase and args use camelCase keys; normalize both
    // into the shared `tool_activity` vocabulary before delegating.
    if status == Status::Running && event == "tool.execute" {
        let raw_tool = v.get("tool").and_then(|x| x.as_str()).unwrap_or("");
        let raw_input = v.get("tool_input").unwrap_or(&Value::Null);
        let tool = normalize_tool_name(raw_tool);
        let tool_input = normalize_tool_args(raw_input);
        if let Some(activity) = tool_activity(tool, &tool_input) {
            out_msg = activity;
        }
    }

    // The sticky task label rides on chat.message (the user-prompt-submit
    // event); every other event sends task=None (keep the stored label).
    let task = if status == Status::Running && event == "chat.message" {
        v.get("prompt").and_then(|x| x.as_str()).and_then(super::task_from_prompt)
    } else {
        None
    };

    Some(AgentUpdate {
        status,
        msg: out_msg,
        cwd,
        task,
    })
}

/// Map opencode's lowercase tool names to the shared `tool_activity`
/// vocabulary. opencode built-ins (`read`, `bash`, …) and common spelling
/// variants are covered; an unknown name passes through unchanged (it falls
/// to the `_` arm of `tool_activity`, yielding `None` → the `working`
/// baseline). opencode keys MCP tools `<server>_<tool>` (no `mcp__` prefix),
/// so they are indistinguishable from unknown names here and read as the
/// baseline too.
fn normalize_tool_name(raw: &str) -> &str {
    match raw {
        "read" => "Read",
        "write" | "create" | "save" => "Write",
        "edit" | "str_replace" | "update" => "Edit",
        "bash" | "execute" => "Bash",
        "grep" => "Grep",
        "glob" | "list" => "Glob",
        "webfetch" | "fetch" => "WebFetch",
        "websearch" | "search" => "WebSearch",
        "task" | "subtask" => "Task",
        "todowrite" | "todo" => "TodoWrite",
        "applypatch" | "apply_patch" => "apply_patch",
        _ => raw,
    }
}

/// Rename opencode's camelCase arg keys into the snake_case keys
/// `tool_activity` reads (`filePath` → `file_path`,
/// `notebookPath` → `notebook_path`). Other keys pass through unchanged. A
/// non-object input is returned as-is.
fn normalize_tool_args(input: &Value) -> Value {
    let Some(obj) = input.as_object() else {
        return input.clone();
    };
    let mut out = serde_json::Map::with_capacity(obj.len());
    for (k, v) in obj {
        let k = match k.as_str() {
            "filePath" => "file_path",
            "notebookPath" => "notebook_path",
            other => other,
        };
        out.insert(k.to_string(), v.clone());
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intake<'a>(raw: &'a str, status_arg: Option<&'a str>) -> Intake<'a> {
        Intake { raw, status_arg }
    }

    #[test]
    fn explicit_status_passes_through() {
        let u = derive(&intake(r#"{"event":"chat.message","prompt":"hi"}"#, Some("running"))).unwrap();
        assert_eq!(u.status, Status::Running);
    }

    #[test]
    fn chat_message_is_running_and_captures_the_task() {
        let u = derive(&intake(
            r#"{"event":"chat.message","prompt":"fix the flaky e2e retries\ndetails…","cwd":"/repo"}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
        assert_eq!(u.task.as_deref(), Some("fix the flaky e2e retries"));
        assert_eq!(u.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn non_prompt_events_never_carry_a_task() {
        let u = derive(&intake(
            r#"{"event":"tool.execute","tool":"read","tool_input":{"filePath":"/p/x.rs"}}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.task, None);
    }

    #[test]
    fn tool_hooks_normalize_names_and_args_into_shared_vocab() {
        // opencode `read` + `filePath` → shared `Read` + `file_path`.
        let u = derive(&intake(
            r#"{"event":"tool.execute","tool":"read","tool_input":{"filePath":"/p/auth.rs"}}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "reading auth.rs");

        // `bash` + `command` → `Bash` activity.
        let u = derive(&intake(
            r#"{"event":"tool.execute","tool":"bash","tool_input":{"command":"git push origin main"}}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.msg, "pushing");

        // `edit` + `filePath`.
        let u = derive(&intake(
            r#"{"event":"tool.execute","tool":"edit","tool_input":{"filePath":"/p/mod.rs"}}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.msg, "editing mod.rs");
    }

    #[test]
    fn unknown_tool_falls_back_to_working() {
        let u = derive(&intake(
            r#"{"event":"tool.execute","tool":"frobnicate","tool_input":{}}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
    }

    #[test]
    fn permission_ask_is_pending_with_title() {
        let u = derive(&intake(
            r#"{"event":"permission.ask","message":"Approve network access?","cwd":"/repo"}"#,
            Some("pending"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "Approve network access?");
        assert_eq!(u.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn permission_replied_resumes_running() {
        // The user answered the prompt: the row leaves ◆ immediately instead of
        // waiting for the next tool/idle event (a denied permission throws
        // inside the tool, so `tool.execute.after` may never come).
        let u = derive(&intake(r#"{"event":"permission.replied","cwd":"/repo"}"#, None)).unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
        assert_eq!(u.task, None);
    }

    #[test]
    fn permission_ask_with_blank_title_is_dropped() {
        // The pending backstop: a permission with no title is not a real
        // "needs you" — drop it rather than paint a generic pending row.
        assert!(derive(&intake(r#"{"event":"permission.ask","message":""}"#, Some("pending"))).is_none());
        assert!(derive(&intake(r#"{"event":"permission.ask","message":"   "}"#, Some("pending"))).is_none());
    }

    #[test]
    fn session_idle_with_statement_stays_done() {
        let u = derive(&intake(
            r#"{"event":"session.idle","message":"All tests pass.","cwd":"/repo"}"#,
            Some("done"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "All tests pass.");
        assert_eq!(u.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn session_idle_ending_in_a_question_remaps_done_to_pending() {
        // A turn that ends mid-question is blocked on the user; only the
        // trailing line rides as the msg.
        let u = derive(&intake(
            r#"{"event":"session.idle","message":"Refactored the auth module.\n\nShould I also update the tests?"}"#,
            Some("done"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "Should I also update the tests?");
        assert_eq!(u.task, None, "no prompt here — keep the stored label");
    }

    #[test]
    fn session_error_maps_to_error_status() {
        // opencode surfaces a real error event (Claude's hook model has none);
        // carry its message, falling back to a neutral label when blank.
        let u = derive(&intake(
            r#"{"event":"session.error","message":"provider auth failed","cwd":"/repo"}"#,
            Some("error"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Error);
        assert_eq!(u.msg, "provider auth failed");
        assert_eq!(u.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn session_error_with_blank_message_gets_neutral_label() {
        let u = derive(&intake(r#"{"event":"session.error","message":""}"#, Some("error"))).unwrap();
        assert_eq!(u.status, Status::Error);
        assert_eq!(u.msg, "errored");
    }

    #[test]
    fn session_lifecycle_resets_to_idle_with_blank_msg() {
        // session.created/deleted → idle: any stale message is dropped so the
        // rail never shows an idle row with leftover text.
        let u = derive(&intake(
            r#"{"event":"session.lifecycle","message":"stale","cwd":"/repo"}"#,
            Some("idle"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Idle);
        assert_eq!(u.msg, "");
        assert_eq!(u.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn running_with_empty_msg_falls_back_to_working() {
        let u = derive(&intake(r#"{"event":"chat.message"}"#, Some("running"))).unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
    }

    #[test]
    fn derives_status_from_event_when_no_explicit_status() {
        // Robustness path: the bridge always passes --status, but deriving
        // from `event` keeps the adapter directly testable without it.
        assert_eq!(
            derive(&intake(r#"{"event":"chat.message"}"#, None)).unwrap().status,
            Status::Running
        );
        assert_eq!(
            derive(&intake(r#"{"event":"session.error","message":"boom"}"#, None)).unwrap().status,
            Status::Error
        );
        assert!(derive(&intake(r#"{"event":"unknown"}"#, None)).is_none());
    }

    #[test]
    fn cwd_absent_is_none() {
        let u = derive(&intake(r#"{"event":"session.idle","message":"done"}"#, Some("done"))).unwrap();
        assert_eq!(u.cwd, None);
    }
}
