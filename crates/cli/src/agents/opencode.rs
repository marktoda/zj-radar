//! Opencode plugin bridge → Radar status update.
//!
//! The opencode producer is a thin JS bridge — `setup/opencode_plugin.js` for
//! opencode 1.x (a server plugin), `setup/opencode_tui_plugin.js` for 2.x (a
//! TUI plugin) — that serializes each hook/bus-event payload and spawns
//! `zj-radar notify opencode --status <s>` with JSON on stdin. The bridge
//! picks the status class (it knows the event); the refinements keyed off
//! the payload's `event` field — the pending backstop, the running baseline,
//! tool-activity substitution, the sticky task capture, and the
//! trailing-question Done→Pending remap — are `agents::derive_bridged`'s,
//! shared with pi. This module supplies opencode's event names and its tool
//! names/args for normalizing into the shared `tool_activity` vocabulary.
//! The `event` names are zj-radar's own wire vocabulary, shared by both
//! bridges, so an opencode API change lands in JS only. `session.error` maps
//! to `Status::Error` — a real failure signal Claude's hook model deliberately
//! lacks (see the claude.rs header comment).

use super::{AgentUpdate, Bridge, Intake};
use crate::status::Status;

/// Map an opencode event name to a status, used when `--status` is absent (the
/// bridge always passes `--status`, so this is the robustness/test path).
fn status_from_event(event: &str) -> Option<Status> {
    match event {
        "chat.message" | "tool.execute" | "needs_you.replied" | "session.execution" => Some(Status::Running),
        "permission.ask" | "question.ask" => Some(Status::Pending),
        "session.idle" => Some(Status::Done),
        "session.error" => Some(Status::Error),
        "session.lifecycle" => Some(Status::Idle),
        _ => None,
    }
}

/// opencode's half of the shared bridge derivation. Tool ids are opencode's
/// built-ins (all lowercase; 1.x `packages/opencode/src/tool/registry.ts`,
/// 2.x `packages/core/src/tool/` where `bash` became `shell` and `task`
/// became `subagent`); MCP tools, keyed `<server>_<tool>`, pass through to
/// the `working` baseline. Args use camelCase keys.
const BRIDGE: Bridge = Bridge {
    status_from_event,
    tool_event: "tool.execute",
    prompt_event: "chat.message",
    tool_names: &[
        ("read", "Read"),
        ("write", "Write"),
        ("edit", "Edit"),
        ("bash", "Bash"),
        ("shell", "Bash"),
        ("grep", "Grep"),
        ("glob", "Glob"),
        ("webfetch", "WebFetch"),
        ("websearch", "WebSearch"),
        ("task", "Task"),
        ("subagent", "Task"),
        ("todowrite", "TodoWrite"),
    ],
    arg_keys: &[("filePath", "file_path"), ("notebookPath", "notebook_path")],
};

/// Decide opencode's status + msg + cwd via `agents::derive_bridged` (the
/// pending backstop, running baseline, tool activity, task capture on
/// `chat.message`, and the trailing-question remap of `session.idle`).
/// Returns `None` for a no-op.
pub fn derive(intake: &Intake) -> Option<AgentUpdate> {
    super::derive_bridged(intake, &BRIDGE)
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

        // opencode 2.x renamed `bash` → `shell` (and `task` → `subagent`);
        // both spellings land on the same shared activity.
        let u = derive(&intake(
            r#"{"event":"tool.execute","tool":"shell","tool_input":{"command":"git push origin main"}}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.msg, "pushing");
        let u =
            derive(&intake(r#"{"event":"tool.execute","tool":"subagent","tool_input":{}}"#, Some("running"))).unwrap();
        assert_eq!(u.msg, "delegating");

        // The 2.x bridge's bare run-started refresh: running, baseline msg.
        let u = derive(&intake(r#"{"event":"session.execution"}"#, None)).unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");

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
        let u = derive(&intake(r#"{"event":"tool.execute","tool":"frobnicate","tool_input":{}}"#, Some("running")))
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
    fn question_ask_is_pending_and_any_needs_you_reply_resumes_running() {
        // opencode's built-in `question` tool blocks the TUI like a permission
        // prompt; the bridge sends it as `question.ask`, and every reply edge
        // (permission.replied / question.replied / question.rejected) arrives
        // as one `needs_you.replied` running.
        let u = derive(&intake(r#"{"event":"question.ask","message":"Which auth strategy?"}"#, None)).unwrap();
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "Which auth strategy?");
        let u = derive(&intake(r#"{"event":"needs_you.replied"}"#, None)).unwrap();
        assert_eq!(u.status, Status::Running);
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
        let u = derive(&intake(r#"{"event":"session.idle","message":"All tests pass.","cwd":"/repo"}"#, Some("done")))
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
        let u =
            derive(&intake(r#"{"event":"session.lifecycle","message":"stale","cwd":"/repo"}"#, Some("idle"))).unwrap();
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
        assert_eq!(derive(&intake(r#"{"event":"chat.message"}"#, None)).unwrap().status, Status::Running);
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
