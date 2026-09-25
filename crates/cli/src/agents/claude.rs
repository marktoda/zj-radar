//! Claude Code hook payload → Radar status update.

mod background;
pub(crate) mod bg_agents;

use super::{tool_activity, AgentUpdate, Intake};
use crate::status::Status;
use serde_json::Value;

const GENERIC_PENDING: [&str; 2] = ["Claude needs attention", "Claude Code needs your attention"];

/// Map a Claude hook event name to a status (used when `--status` is absent).
///
/// `error` is deliberately absent: Claude's hook vocabulary has no reliable
/// failure signal. `PostToolUse` carries per-tool `is_error`, but a mid-turn
/// tool failure is normal agent behavior (it recovers and continues), and
/// `Stop` reports no turn-level outcome — so mapping either to `Error` would
/// paint healthy turns red. Observed command exits remain the only error
/// source until the hook model grows a real one.
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
    let status = match intake.status_arg {
        Some(s) => Status::from_wire(s),
        None => status_from_event(event?)?,
    };

    // Only a Stop (a Done) reads `last_assistant_message`: a SubagentStop
    // carries the subagent's whole final report there, which must not become
    // the running row's msg.
    let msg = v
        .get("message")
        .and_then(|x| x.as_str())
        .or_else(|| {
            (status == Status::Done)
                .then(|| v.get("last_assistant_message").and_then(|x| x.as_str()))
                .flatten()
        })
        .unwrap_or("");

    let cwd = v.get("cwd").and_then(|x| x.as_str()).map(str::to_string);

    // A Done is a `Stop`: the turn is over. Every Stop carries the turn-end
    // snapshot of background tasks (`background::turn_end`), whichever status
    // it resolves to below.
    if status == Status::Done {
        let snapshot = background::turn_end(&v);
        // A turn that ends by asking the user something is blocked on input,
        // not finished — but Claude's hook model only surfaces tool-permission
        // questions as `Notification`s; a prose question just fires `Stop`.
        // Remap that Done to Pending with the trailing question as the
        // message, so the rail shows "needs you" instead of a green row the
        // user will misread as safe to ignore.
        if let Some(question) = super::trailing_question(msg) {
            return Some(AgentUpdate {
                status: Status::Pending,
                msg: question.to_string(),
                cwd,
                task: None,
                tasks: Some(snapshot),
            });
        }
        // The turn ended, but work it backgrounded (tests, a subagent) is
        // still running and will wake the model when it finishes — the next
        // `Stop` carries the refreshed list, so the real Done arrives then.
        if let Some(waiting) = background::waiting_msg(&snapshot) {
            return Some(AgentUpdate {
                status: Status::Running,
                msg: waiting,
                cwd,
                task: None,
                tasks: Some(snapshot),
            });
        }
        return Some(AgentUpdate {
            status,
            msg: super::baseline_msg(status, msg),
            cwd,
            task: None,
            tasks: Some(snapshot),
        });
    }

    if status == Status::Pending {
        let m = msg.trim();
        if m.is_empty() || GENERIC_PENDING.contains(&m) {
            return None; // backstop: not a real "needs you"
        }
    }

    // The shared producer baseline (`agents::baseline_msg`): running-but-blank
    // gets `working`, idle always broadcasts blank. The tool-activity
    // substitution below refines the running case when a tool name/input is
    // present.
    let mut out_msg = super::baseline_msg(status, msg);

    // For PreToolUse/PostToolUse, show the live action instead of the baseline.
    if status == Status::Running && matches!(event, Some("PreToolUse") | Some("PostToolUse")) {
        let tool_name = v.get("tool_name").and_then(|x| x.as_str()).unwrap_or("");
        let tool_input = v.get("tool_input").unwrap_or(&Value::Null);
        if let Some(activity) = tool_activity(tool_name, tool_input) {
            out_msg = activity;
        }
    }

    let prompt = v.get("prompt").and_then(|x| x.as_str());
    let (task, tasks) = match event {
        Some("UserPromptSubmit") => (prompt.and_then(super::task_from_prompt), prompt.and_then(background::ended)),
        Some("PostToolUse") => (None, background::started(&v)),
        _ => (None, None),
    };
    Some(AgentUpdate {
        status,
        msg: out_msg,
        cwd,
        task,
        tasks,
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
    fn stop_ending_in_a_question_remaps_done_to_pending() {
        // A turn that ends mid-question is blocked on the user; "done" would
        // read as safe to ignore. Only the trailing line rides as the msg —
        // it's the sentence the rail's question slot can actually show.
        let u = derive(&intake(
            r#"{"hook_event_name":"Stop","last_assistant_message":"Refactored the auth module.\n\nShould I also update the tests?","cwd":"/home/u/repo"}"#,
            Some("done"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "Should I also update the tests?");
        assert_eq!(u.cwd.as_deref(), Some("/home/u/repo"));
        assert_eq!(u.task, None, "no prompt here — keep the stored label");
    }

    #[test]
    fn stop_ending_in_a_statement_stays_done() {
        // A question mark anywhere but the trailing line is not "ends by
        // asking" — the turn finished on a statement.
        let u = derive(&intake(
            r#"{"hook_event_name":"Stop","last_assistant_message":"Want anything else?\nAll tests pass."}"#,
            Some("done"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "Want anything else?\nAll tests pass.");
    }

    /// A `Stop` payload shaped like Claude Code 2.1.282's live capture:
    /// `background_tasks` lists what the session still has running.
    fn stop_with_tasks(tasks: &str) -> String {
        format!(
            r#"{{"hook_event_name":"Stop","last_assistant_message":"started","cwd":"/home/u/repo","background_tasks":{tasks},"session_crons":[]}}"#
        )
    }

    #[test]
    fn stop_with_a_running_background_shell_stays_running() {
        // The turn ended but the tests it backgrounded haven't: the agent will
        // be woken when they finish, so the row is not done yet.
        let raw = stop_with_tasks(
            r#"[{"id":"bks7shfy0","type":"shell","status":"running","description":"Run the test suite","command":"cargo nextest run"}]"#,
        );
        let u = derive(&intake(&raw, Some("done"))).unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "waiting on Run the test suite");
        assert_eq!(u.cwd.as_deref(), Some("/home/u/repo"));
        assert_eq!(u.task, None, "keep the stored label");
    }

    #[test]
    fn stop_with_several_holding_tasks_counts_them() {
        let raw = stop_with_tasks(
            r#"[{"id":"b1","type":"shell","status":"running","description":"tests","command":"pytest"},
                {"id":"a1","type":"subagent","status":"running","description":"Explore rail","agent_type":"Explore"}]"#,
        );
        let u = derive(&intake(&raw, Some("done"))).unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "waiting on 2 tasks");
    }

    #[test]
    fn stop_with_a_background_subagent_or_workflow_stays_running() {
        for ty in ["subagent", "workflow", "teammate"] {
            let raw = stop_with_tasks(&format!(
                r#"[{{"id":"a1","type":"{ty}","status":"running","description":"Explore rail"}}]"#
            ));
            let u = derive(&intake(&raw, Some("done"))).unwrap();
            assert_eq!(u.status, Status::Running, "{ty} holds the agent");
        }
    }

    #[test]
    fn a_task_without_a_description_falls_back_to_its_command_then_a_count() {
        let raw = stop_with_tasks(r#"[{"id":"b1","type":"shell","status":"running","command":"make"}]"#);
        let u = derive(&intake(&raw, Some("done"))).unwrap();
        assert_eq!(u.msg, "waiting on make");
        let raw = stop_with_tasks(r#"[{"id":"w1","type":"workflow","status":"running"}]"#);
        let u = derive(&intake(&raw, Some("done"))).unwrap();
        assert_eq!(u.msg, "waiting on 1 task");
    }

    #[test]
    fn every_stop_carries_the_turn_end_snapshot() {
        // Done, waiting-Running, and question-Pending all send it; a Stop
        // with no field sends an empty one so nothing stale outlives a turn.
        let waiting = derive(&intake(&stop_with_tasks(r#"[{"id":"b1","type":"shell","status":"running","command":"pytest"}]"#), Some("done"))).unwrap();
        let t = waiting.tasks.unwrap();
        assert!(t.snapshot);
        assert_eq!((t.items[0].id.as_str(), t.items[0].holds), ("b1", true));
        let done = derive(&intake(r#"{"hook_event_name":"Stop","last_assistant_message":"ok"}"#, Some("done"))).unwrap();
        assert_eq!(done.status, Status::Done);
        let t = done.tasks.unwrap();
        assert!(t.snapshot && t.items.is_empty());
        let asked = derive(&intake(r#"{"hook_event_name":"Stop","last_assistant_message":"Ship it?","background_tasks":[{"id":"d","type":"shell","status":"running","command":"npm run dev"}]}"#, Some("done"))).unwrap();
        assert_eq!(asked.status, Status::Pending);
        assert!(!asked.tasks.unwrap().items[0].holds, "a dev server never holds");
    }

    #[test]
    fn a_background_launch_reports_its_start() {
        let u = derive(&intake(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"cargo test","description":"Run tests","run_in_background":true},"tool_response":{"backgroundTaskId":"b7"}}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "running tests", "the live activity still shows");
        let t = u.tasks.unwrap();
        assert!(!t.snapshot);
        assert_eq!((t.items[0].id.as_str(), t.items[0].label.as_str()), ("b7", "Run tests"));
        // The PreToolUse twin and ordinary tools carry nothing.
        let pre = derive(&intake(r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"x","run_in_background":true}}"#, Some("running"))).unwrap();
        assert_eq!(pre.tasks, None);
    }

    #[test]
    fn a_task_notification_wake_reports_outcomes_and_keeps_the_label() {
        let u = derive(&intake(
            r#"{"hook_event_name":"UserPromptSubmit","prompt":"<task-notification>\n<task-id>b7</task-id>\n<status>failed</status>\n</task-notification>"}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.task, None, "machinery, not the human's task");
        let t = u.tasks.unwrap();
        assert_eq!((t.items[0].id.as_str(), t.items[0].state), ("b7", zj_radar_core::task::TaskState::Failed));
        let human = derive(&intake(r#"{"hook_event_name":"UserPromptSubmit","prompt":"fix it"}"#, Some("running"))).unwrap();
        assert_eq!(human.tasks, None);
    }

    #[test]
    fn services_and_finished_tasks_do_not_hold_the_agent() {
        // A dev server, a `tail -f`, a monitor, or an unknown task type may
        // never end — holding on them would spin the row forever. A task
        // whose status isn't `running` is already over.
        let raw = stop_with_tasks(
            r#"[{"id":"b1","type":"shell","status":"running","description":"dev server","command":"cd web && pnpm run dev"},
                {"id":"b2","type":"shell","status":"running","description":"follow log","command":"tail -F app.log"},
                {"id":"b3","type":"shell","status":"running","description":"stack","command":"docker compose up"},
                {"id":"b4","type":"shell","status":"running","description":"docs","command":"mkdocs serve"},
                {"id":"m1","type":"monitor","status":"running","description":"watch CI"},
                {"id":"x1","type":"something_new","status":"running","description":"?"},
                {"id":"b5","type":"shell","status":"completed","description":"tests","command":"cargo test"}]"#,
        );
        let u = derive(&intake(&raw, Some("done"))).unwrap();
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "started");
    }

    #[test]
    fn stop_without_or_with_empty_background_tasks_is_done() {
        // Older Claude Code sends no field at all; a malformed one is ignored.
        for tasks in ["[]", "null", r#""oops""#, r#"[{"type":"shell"}]"#] {
            let raw = stop_with_tasks(tasks);
            let u = derive(&intake(&raw, Some("done"))).unwrap();
            assert_eq!(u.status, Status::Done, "background_tasks = {tasks}");
        }
        let u = derive(&intake(r#"{"hook_event_name":"Stop","message":"done"}"#, None)).unwrap();
        assert_eq!(u.status, Status::Done);
    }

    #[test]
    fn a_trailing_question_outranks_background_work() {
        // Blocked on the user beats waiting on a task: the row needs you.
        let raw = r#"{"hook_event_name":"Stop","last_assistant_message":"Tests are running. Want me to also bump the version?","background_tasks":[{"id":"b1","type":"shell","status":"running","command":"cargo test"}]}"#;
        let u = derive(&intake(raw, Some("done"))).unwrap();
        assert_eq!(u.status, Status::Pending);
    }

    #[test]
    fn background_tasks_only_matter_on_a_done() {
        // A tool hook never carries the field today; if one ever did, it must
        // not rewrite the live activity.
        let raw = r#"{"hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"/p/x.rs"},"background_tasks":[{"id":"b1","type":"shell","status":"running","command":"cargo test"}]}"#;
        let u = derive(&intake(raw, Some("running"))).unwrap();
        assert_eq!(u.msg, "editing x.rs");
    }

    #[test]
    fn a_subagents_tool_hooks_still_derive() {
        // derive is stateless: a (foreground) subagent's tool calls keep
        // reporting — its PostToolUse is the Pending-recovery edge after a
        // permission answered inside it. Background subagents are filtered
        // before derive, by `bg_agents` (it needs per-pane state).
        let raw = r#"{"hook_event_name":"PostToolUse","agent_id":"a55dd2e69238dcfae","agent_type":"general-purpose","tool_name":"Read","tool_input":{"file_path":"/p/x.rs"}}"#;
        assert_eq!(derive(&intake(raw, Some("running"))).unwrap().msg, "reading x.rs");
    }

    #[test]
    fn subagent_stop_ignores_the_subagents_final_report() {
        // A SubagentStop's `last_assistant_message` is the subagent's whole
        // report; only a Stop reads the field.
        let raw = r###"{"hook_event_name":"SubagentStop","agent_id":"a1","last_assistant_message":"## Findings\n\n- x"}"###;
        for status_arg in [Some("running"), None] {
            let u = derive(&intake(raw, status_arg)).unwrap();
            assert_eq!((u.status, u.msg.as_str()), (Status::Running, "working"), "{status_arg:?}");
        }
    }

    #[test]
    fn agent_tool_reads_as_delegating() {
        // Current Claude Code names the subagent tool `Agent` (was `Task`).
        let u = derive(&intake(
            r#"{"hook_event_name":"PreToolUse","tool_name":"Agent","tool_input":{"prompt":"x","run_in_background":true}}"#,
            None,
        ))
        .unwrap();
        assert_eq!(u.msg, "delegating");
    }

    #[test]
    fn clear_session_resets_to_idle() {
        // `/clear` fires SessionStart{source:"clear"}; the plugin wires it to an
        // explicit `idle` status. With no message in the payload it yields a
        // blank idle update — the row recedes instead of keeping its stale msg.
        let u = derive(&intake(
            r#"{"hook_event_name":"SessionStart","source":"clear","cwd":"/home/u/repo"}"#,
            Some("idle"),
        ))
        .unwrap();
        assert_eq!(u.status, Status::Idle);
        assert_eq!(u.msg, "");
        assert_eq!(u.cwd.as_deref(), Some("/home/u/repo"));
    }

    #[test]
    fn idle_status_clears_any_message() {
        // idle means "no activity": any message riding along the payload (e.g. a
        // SessionStart session_title, or a stale last_assistant_message) is
        // dropped so the rail never shows an idle row with leftover text.
        let u = derive(&intake(r#"{"message":"old work in progress"}"#, Some("idle"))).unwrap();
        assert_eq!(u.status, Status::Idle);
        assert_eq!(u.msg, "");
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

    #[test]
    fn user_prompt_submit_captures_the_task() {
        let u = derive(&intake(
            r#"{"hook_event_name":"UserPromptSubmit","prompt":"fix the flaky e2e retries\ndetails…"}"#,
            Some("running"),
        ))
        .unwrap();
        assert_eq!(u.task.as_deref(), Some("fix the flaky e2e retries"));
        assert_eq!(u.msg, "working");
    }

    #[test]
    fn non_prompt_events_never_carry_a_task() {
        // A tool hook or Stop must send task=None (wire: empty = keep stored).
        let u = derive(&intake(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Edit","tool_input":{"file_path":"/p/x.rs"},"prompt":"stray"}"#,
            None,
        ))
        .unwrap();
        assert_eq!(u.task, None);
        let u = derive(&intake(r#"{"hook_event_name":"Stop","message":"done"}"#, None)).unwrap();
        assert_eq!(u.task, None);
    }
}
