//! Claude's background tasks → the wire's `tasks` batches (`core::task`).
//!
//! Three hook moments carry them, each self-contained, so the producer stays
//! stateless (verified live on Claude Code 2.1.282):
//!
//! - **start** — `PostToolUse` whose `tool_response` has a `backgroundTaskId`
//!   (a backgrounded shell) or `status: "async_launched"` + `agentId` (a
//!   background subagent): one running upsert ([`started`]).
//! - **end** — the harness wakes the model with a `UserPromptSubmit` whose
//!   prompt is one or more `<task-notification>` blocks carrying
//!   `<task-id>` and `<status>completed|failed|killed</status>` ([`ended`]).
//!   Fires at the next tool boundary even mid-turn.
//! - **turn end** — `Stop` lists what is still running in
//!   `background_tasks: [{id, type, status, description, command?}]`: the
//!   authoritative snapshot ([`turn_end`]), sent on *every* Stop (an absent or
//!   malformed field is an empty snapshot, so nothing stale survives a turn).
//!
//! The same ids flow through all three (a subagent's `agentId` is its
//! `background_tasks` id and its notification `task-id`). One known gap:
//! work a foreground *subagent* backgrounds fires its start on the parent's
//! pane, but the parent's `Stop` likely doesn't list it and its outcome wakes
//! the subagent, so the rail can end it as a muted `·` while it still runs —
//! never a false green, so accepted. (A *background* subagent's hooks,
//! launches included, are dropped before derive — `bg_agents`.)
//!
//! Task lines are CLI-only: notify.sh's bash fallback sends no `tasks`, and
//! mirrors only the resulting waiting status (the "waiting on …" Running of a
//! turn end); parity.bats pins that status, and `SERVICE_PHRASES`, between
//! the two.

use crate::agents::{command_basename, shell_is_service};
use serde_json::Value;
use zj_radar_core::task::{TaskBatch, TaskState, TaskUpdate};

/// Display label: the model-written `description` (Claude nearly always sets
/// one), else the command's first-token basename (`bash_activity`'s fallback
/// rule), else nothing — an empty label keeps whatever the rail stored.
fn label(description: Option<&str>, command: Option<&str>) -> String {
    if let Some(d) = description.map(str::trim).filter(|d| !d.is_empty()) {
        return d.to_string();
    }
    command.and_then(command_basename).unwrap_or("").to_string()
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The turn-end snapshot from a `Stop` payload: every *running* task, holding
/// or not. Holding = bounded work whose end will wake the agent: subagents,
/// workflows, teammates, and shells whose command or description doesn't
/// look like a service (`shell_is_service`; a shell with neither doesn't
/// hold, and the plugin never re-raises a stored `holds: false`). Monitors,
/// crons, services and unknown types are listed but never hold — a row must
/// not spin forever on work that may never end. Uncapped here: `to_wire`
/// keeps the first `MAX_TASKS`, so past 16 live tasks the rail ends the rest
/// as `Ended` and "waiting on N tasks" counts more than it draws — accepted,
/// no real session runs that many.
pub(super) fn turn_end(v: &Value) -> TaskBatch {
    let items = v
        .get("background_tasks")
        .and_then(Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .filter(|t| str_field(t, "status") == Some("running"))
                .filter_map(|t| {
                    let id = str_field(t, "id").filter(|id| !id.is_empty())?;
                    let (command, description) = (str_field(t, "command"), str_field(t, "description"));
                    let holds = match str_field(t, "type") {
                        Some("subagent" | "workflow" | "teammate") => true,
                        Some("shell") => !shell_is_service(command, description),
                        _ => false,
                    };
                    Some(TaskUpdate {
                        id: id.to_string(),
                        state: TaskState::Running,
                        label: label(description, command),
                        holds,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    TaskBatch { items, snapshot: true }
}

/// The Running msg for a turn end still waiting on holding work, or `None`.
pub(super) fn waiting_msg(snapshot: &TaskBatch) -> Option<String> {
    let holding: Vec<&TaskUpdate> = snapshot.items.iter().filter(|t| t.holds).collect();
    match holding.as_slice() {
        [] => None,
        [only] if !only.label.is_empty() => Some(format!("waiting on {}", only.label)),
        [_] => Some("waiting on 1 task".to_string()),
        many => Some(format!("waiting on {} tasks", many.len())),
    }
}

/// A start upsert from a `PostToolUse` that launched background work.
pub(super) fn started(v: &Value) -> Option<TaskBatch> {
    let response = v.get("tool_response")?;
    let input = v.get("tool_input").unwrap_or(&Value::Null);
    let description = str_field(input, "description");
    let update = if let Some(id) = str_field(response, "backgroundTaskId") {
        let command = str_field(input, "command");
        TaskUpdate {
            id: id.to_string(),
            state: TaskState::Running,
            label: label(description, command),
            holds: !shell_is_service(command, description),
        }
    } else if str_field(response, "status") == Some("async_launched") {
        TaskUpdate {
            id: str_field(response, "agentId")?.to_string(),
            state: TaskState::Running,
            label: label(description, None),
            holds: true,
        }
    } else {
        return None;
    };
    (!update.id.is_empty()).then(|| TaskBatch { items: vec![update], snapshot: false })
}

/// The text between `<tag>` and `</tag>` in `block`, if present.
fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = block.find(&open)? + open.len();
    let len = block[start..].find(&format!("</{name}>"))?;
    Some(block[start..start + len].trim())
}

/// Outcome upserts from a harness wake prompt: one per `<task-notification>`
/// block. Only a prompt that *opens* with the tag is a wake — a human pasting
/// the text mid-prompt isn't one. A status other than completed/failed/killed
/// skips the block (never guess an outcome).
pub(super) fn ended(prompt: &str) -> Option<TaskBatch> {
    if !prompt.trim_start().starts_with("<task-notification>") {
        return None;
    }
    let items: Vec<TaskUpdate> = prompt
        .split("<task-notification>")
        .skip(1)
        .filter_map(|block| {
            let id = tag(block, "task-id").filter(|id| !id.is_empty())?;
            let state = match tag(block, "status")? {
                s @ ("completed" | "failed" | "killed") => TaskState::from_wire(s)?,
                _ => return None,
            };
            Some(TaskUpdate { id: id.to_string(), state, label: String::new(), holds: false })
        })
        .collect();
    (!items.is_empty()).then_some(TaskBatch { items, snapshot: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn turn_end_lists_every_running_task_and_marks_what_holds() {
        let v = json(r#"{"background_tasks":[
            {"id":"b1","type":"shell","status":"running","description":" Run the test suite ","command":"cargo nextest run"},
            {"id":"b2","type":"shell","status":"running","command":"cd web && pnpm run dev"},
            {"id":"a1","type":"subagent","status":"running","description":"Explore rail"},
            {"id":"m1","type":"monitor","status":"running","description":"watch CI"},
            {"id":"b3","type":"shell","status":"completed","command":"cargo test"},
            {"type":"shell","status":"running"},"junk"]}"#);
        let batch = turn_end(&v);
        assert!(batch.snapshot);
        let got: Vec<(&str, &str, bool)> =
            batch.items.iter().map(|t| (t.id.as_str(), t.label.as_str(), t.holds)).collect();
        assert_eq!(
            got,
            vec![("b1", "Run the test suite", true), ("b2", "cd", false), ("a1", "Explore rail", true), ("m1", "watch CI", false)]
        );
    }

    #[test]
    fn turn_end_without_the_field_is_an_empty_snapshot() {
        for raw in [r#"{}"#, r#"{"background_tasks":null}"#, r#"{"background_tasks":"x"}"#] {
            let batch = turn_end(&json(raw));
            assert!(batch.snapshot && batch.items.is_empty(), "{raw}");
        }
    }

    #[test]
    fn waiting_msg_names_one_task_or_counts_several() {
        let one = turn_end(&json(r#"{"background_tasks":[{"id":"b1","type":"shell","status":"running","command":"make"}]}"#));
        assert_eq!(waiting_msg(&one).as_deref(), Some("waiting on make"));
        let unnamed = turn_end(&json(r#"{"background_tasks":[{"id":"w","type":"workflow","status":"running"}]}"#));
        assert_eq!(waiting_msg(&unnamed).as_deref(), Some("waiting on 1 task"));
        let services = turn_end(&json(r#"{"background_tasks":[{"id":"d","type":"shell","status":"running","command":"npm run dev"}]}"#));
        assert_eq!(waiting_msg(&services), None);
    }

    fn shell_holds(command: Option<&str>, description: Option<&str>) -> bool {
        let mut t = serde_json::json!({"id": "b", "type": "shell", "status": "running"});
        if let Some(c) = command {
            t["command"] = c.into();
        }
        if let Some(d) = description {
            t["description"] = d.into();
        }
        turn_end(&serde_json::json!({ "background_tasks": [t] })).items[0].holds
    }

    #[test]
    fn long_running_servers_watchers_and_tunnels_never_hold() {
        for cmd in [
            "python -m http.server 8000", "make dev", "just dev", "npx vite", "uvicorn app:app --reload",
            "flask run", "rails s", "bin/rails server", "python manage.py runserver", "gunicorn app:wsgi",
            "cargo watch -x test", "tsc --watch", "kubectl port-forward svc/db 5432", "nodemon index.js",
            "bundle exec jekyll serve", "make server", "vite", "npx vite dev", "./node_modules/.bin/vite serve",
            "pnpm vite preview", "jest --watch", "jest --watchAll", "vitest --watch=true", "watchexec -e rs cargo test",
        ] {
            assert!(!shell_holds(Some(cmd), None), "{cmd} is a service");
        }
    }

    #[test]
    fn bounded_uses_of_single_service_words_hold() {
        // A false service is permanent (`holds` only drops) and may notify
        // "finished" early, so these single words need the right position.
        for cmd in [
            "npx vite build", "vite build --mode prod", "jest --watch=false", "vitest --watch=0", "gh run watch 123",
            "./watch.sh", "vitest run", "npm run watch-docs-check",
        ] {
            assert!(shell_holds(Some(cmd), None), "{cmd} is bounded");
        }
    }

    #[test]
    fn the_description_can_mark_a_service_but_single_words_do_not() {
        // An unlisted command whose description gives it away.
        assert!(!shell_holds(Some("./bin/app --port 3000"), Some("Start the dev server")));
        assert!(!shell_holds(Some("./run.sh"), Some("Rebuild in watch mode")));
        // Bounded work whose description mentions a service word in passing.
        for (cmd, desc) in [
            ("cargo test -p server", "Run the server tests"),
            ("go build ./cmd/server", "Build the server"),
            ("pytest tests/test_watcher.py", "Test the file watcher"),
            ("cargo test", "Run tests and watch for failures"),
            ("npm test", "Serve up the test report"),
            ("vitest run", "Run unit tests"),
            ("./observe.sh", "Observe results"),
        ] {
            assert!(shell_holds(Some(cmd), Some(desc)), "{cmd} / {desc} is bounded");
        }
    }

    #[test]
    fn a_shell_without_a_command_falls_back_to_its_description() {
        assert!(shell_holds(None, Some("Run the test suite")));
        assert!(shell_holds(Some("  "), Some("Run the test suite")));
        assert!(!shell_holds(None, Some("Start the dev server")));
        // Nothing says it's bounded: don't hold.
        assert!(!shell_holds(None, None));
        assert!(!shell_holds(Some(""), Some(" ")));
    }

    #[test]
    fn started_reads_a_backgrounded_shell_and_a_background_agent() {
        // Shapes from the live capture.
        let shell = started(&json(r#"{"tool_input":{"command":"sleep 25; echo BG_DONE","description":"Background sleep 25 seconds","run_in_background":true},
            "tool_response":{"stdout":"","backgroundTaskId":"bks7shfy0"}}"#)).unwrap();
        assert_eq!(shell.items[0], TaskUpdate { id: "bks7shfy0".into(), state: TaskState::Running, label: "Background sleep 25 seconds".into(), holds: true });
        assert!(!shell.snapshot);
        let agent = started(&json(r#"{"tool_input":{"description":"Sleep and echo in subagent","run_in_background":true},
            "tool_response":{"isAsync":true,"status":"async_launched","agentId":"a7c69e57f79598a8a"}}"#)).unwrap();
        assert_eq!((agent.items[0].id.as_str(), agent.items[0].holds), ("a7c69e57f79598a8a", true));
        assert_eq!(started(&json(r#"{"tool_response":{"stdout":"hi"}}"#)), None);
        assert_eq!(started(&json(r#"{"tool_response":{"backgroundTaskId":""}}"#)), None);
    }

    #[test]
    fn ended_reads_every_notification_block() {
        let prompt = "<task-notification>\n<task-id>b9pmfbej6</task-id>\n<status>failed</status>\n<summary>Background command \"x\" failed with exit code 3</summary>\n</task-notification>\n\
                      <task-notification>\n<task-id>b3dea3w6o</task-id>\n<status>completed</status>\n</task-notification>\n\
                      <task-notification>\n<task-id>m1</task-id>\n<status>event</status>\n</task-notification>";
        let batch = ended(prompt).unwrap();
        let got: Vec<(&str, TaskState)> = batch.items.iter().map(|t| (t.id.as_str(), t.state)).collect();
        assert_eq!(got, vec![("b9pmfbej6", TaskState::Failed), ("b3dea3w6o", TaskState::Completed)]);
        assert_eq!(ended("fix the <task-notification> parser"), None, "a human prompt isn't a wake");
        assert_eq!(ended("<task-notification><status>killed</status></task-notification>"), None);
    }
}
