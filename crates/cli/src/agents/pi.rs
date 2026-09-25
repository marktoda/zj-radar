//! pi extension bridge → Radar status update.
//!
//! The pi producer is a thin JS bridge — `setup/pi_extension.js`, vendored
//! into pi's auto-loaded extensions dir — that translates pi extension events
//! into zj-radar's own wire vocabulary and spawns
//! `zj-radar notify pi --status <s>` with JSON on stdin. The bridge picks the
//! status class (it knows the event and pi's idle state); the refinements
//! keyed off the payload's `event` field — the pending backstop, the running
//! baseline, tool-activity substitution, the sticky task capture, and the
//! trailing-question Done→Pending remap — are `agents::derive_bridged`'s,
//! shared with opencode. This module supplies pi's event names and its
//! lowercase tool names and `path` arg for the shared `tool_activity`
//! vocabulary. The event names are zj-radar's, not pi's, so a pi API change
//! lands in JS only.

use super::{AgentUpdate, Bridge, Intake};
use crate::status::Status;

/// Map a bridge event to a status, used when `--status` is absent (the bridge
/// always passes it; this is the robustness/test path). `ui_prompt.end` has
/// no fixed status — the bridge resolves it from pi's idle state — so it is
/// `None` here.
fn status_from_event(event: &str) -> Option<Status> {
    match event {
        "prompt" | "tool" => Some(Status::Running),
        "ui_prompt.start" => Some(Status::Pending),
        "settled" => Some(Status::Done),
        "error" => Some(Status::Error),
        "session.new" | "session.end" => Some(Status::Idle),
        _ => None,
    }
}

/// pi's half of the shared bridge derivation. Tool names are pi's built-ins
/// (`dist/core/tools/*.js`, all lowercase); `ls` reads as a search, like
/// `find`, and extension tools pass through to the `working` baseline. The
/// file arg is `path`. A dialog with a blank title is a malformed payload —
/// the bridge substitutes "needs input" for untitled kinds — so the shared
/// pending backstop drops it.
const BRIDGE: Bridge = Bridge {
    status_from_event,
    tool_event: "tool",
    prompt_event: "prompt",
    tool_names: &[
        ("read", "Read"),
        ("write", "Write"),
        ("edit", "Edit"),
        ("bash", "Bash"),
        ("powershell", "Bash"),
        ("grep", "Grep"),
        ("find", "Glob"),
        ("ls", "Glob"),
    ],
    arg_keys: &[("path", "file_path")],
};

/// Decide pi's status + msg + cwd via `agents::derive_bridged`. Returns
/// `None` for a no-op.
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
    fn prompt_is_running_and_captures_the_task() {
        let u = derive(&intake(
            r#"{"event":"prompt","prompt":"fix the flaky e2e retries\nlog:","cwd":"/repo"}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
        assert_eq!(u.task.as_deref(), Some("fix the flaky e2e retries"));
        assert_eq!(u.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn prompt_without_text_keeps_the_label() {
        // agent_start with no stashed input (auto-retry, compaction, an
        // extension-sent message) is a bare running refresh.
        let u = derive(&intake(r#"{"event":"prompt"}"#, Some("running"))).unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.task, None);
    }

    #[test]
    fn prompt_ack_keeps_label() {
        let u = derive(&intake(
            r#"{"event":"prompt","prompt":"ok"}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.task, None);
    }

    #[test]
    fn tool_events_normalize_pi_tools_into_shared_vocab() {
        for (raw, expected) in [
            (
                r#"{"event":"tool","tool":"read","tool_input":{"path":"/p/auth.rs"}}"#,
                "reading auth.rs",
            ),
            (
                r#"{"event":"tool","tool":"edit","tool_input":{"path":"/p/mod.rs"}}"#,
                "editing mod.rs",
            ),
            (
                r#"{"event":"tool","tool":"write","tool_input":{"path":"/p/new.rs"}}"#,
                "editing new.rs",
            ),
            (
                r#"{"event":"tool","tool":"bash","tool_input":{"command":"git push origin main"}}"#,
                "pushing",
            ),
            (
                r#"{"event":"tool","tool":"powershell","tool_input":{"command":"cargo test"}}"#,
                "running tests",
            ),
            (
                r#"{"event":"tool","tool":"grep","tool_input":{"pattern":"x"}}"#,
                "searching",
            ),
            (
                r#"{"event":"tool","tool":"find","tool_input":{"pattern":"*.rs"}}"#,
                "searching",
            ),
            (
                r#"{"event":"tool","tool":"ls","tool_input":{"path":"."}}"#,
                "searching",
            ),
        ] {
            let u = derive(&intake(raw, Some("running"))).unwrap();
            assert_eq!(u.status, Status::Running, "{raw}");
            assert_eq!(u.msg, expected, "{raw}");
            assert_eq!(u.task, None, "tool events never carry a task: {raw}");
        }
    }

    #[test]
    fn unknown_tool_falls_back_to_working() {
        let u = derive(&intake(
            r#"{"event":"tool","tool":"frobnicate","tool_input":null}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.msg, "working");
    }

    #[test]
    fn ui_prompt_is_pending_with_title_and_blank_is_dropped() {
        let u = derive(&intake(
            r#"{"event":"ui_prompt.start","message":"Allow rm -rf build?"}"#,
            Some("pending"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "Allow rm -rf build?");
        assert!(derive(&intake(
            r#"{"event":"ui_prompt.start","message":"  "}"#,
            Some("pending")
        ))
        .is_none());
    }

    #[test]
    fn ui_prompt_end_follows_the_status_the_bridge_chose() {
        let u = derive(&intake(r#"{"event":"ui_prompt.end"}"#, Some("running"))).unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
        let u = derive(&intake(r#"{"event":"ui_prompt.end"}"#, Some("idle"))).unwrap();
        assert_eq!(u.status, Status::Idle);
        assert_eq!(u.msg, "");
    }

    #[test]
    fn settled_statement_is_done_and_question_remaps_to_pending() {
        let u = derive(&intake(
            r#"{"event":"settled","message":"All tests pass."}"#,
            Some("done"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "All tests pass.");
        let u = derive(&intake(
            r#"{"event":"settled","message":"Refactored auth.\n\nShould I update the tests?"}"#,
            Some("done"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "Should I update the tests?");
    }

    #[test]
    fn error_carries_message_or_neutral_label() {
        let u = derive(&intake(
            r#"{"event":"error","message":"401 invalid api key"}"#,
            Some("error"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Error);
        assert_eq!(u.msg, "401 invalid api key");
        let u = derive(&intake(r#"{"event":"error","message":""}"#, Some("error"))).unwrap();
        assert_eq!(u.msg, "errored");
    }

    #[test]
    fn session_new_and_end_are_blank_idle() {
        for ev in ["session.new", "session.end"] {
            let raw = format!(r#"{{"event":"{ev}","message":"stale","cwd":"/repo"}}"#);
            let u = derive(&intake(&raw, Some("idle"))).unwrap();
            assert_eq!(u.status, Status::Idle, "{ev}");
            assert_eq!(u.msg, "", "{ev}");
            assert_eq!(u.cwd.as_deref(), Some("/repo"), "{ev}");
        }
    }

    #[test]
    fn derives_status_from_event_when_no_explicit_status() {
        assert_eq!(
            derive(&intake(r#"{"event":"prompt"}"#, None))
                .unwrap()
                .status,
            Status::Running
        );
        assert_eq!(
            derive(&intake(r#"{"event":"settled","message":"ok."}"#, None))
                .unwrap()
                .status,
            Status::Done
        );
        assert_eq!(
            derive(&intake(r#"{"event":"error","message":"x"}"#, None))
                .unwrap()
                .status,
            Status::Error
        );
        assert_eq!(
            derive(&intake(r#"{"event":"session.new"}"#, None))
                .unwrap()
                .status,
            Status::Idle
        );
        assert!(
            derive(&intake(r#"{"event":"ui_prompt.end"}"#, None)).is_none(),
            "the bridge always picks ui_prompt.end's status"
        );
        assert!(derive(&intake(r#"{"event":"unknown"}"#, None)).is_none());
        assert!(derive(&intake("not json", None)).is_none());
    }
}
