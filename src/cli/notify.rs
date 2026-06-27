//! `zj-radar notify <agent>` — derive status, build payload, broadcast.

use crate::payload::to_wire;
use crate::status::Status;
use std::io::Read;
use std::process::Command;

/// The decision a pure derivation produces. `None` from a `derive_*` means no-op.
pub struct Update {
    pub status: Status,
    pub msg: String,
}

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

/// Pure: decide Claude's status+msg. `status_arg` (from the matcher-driven
/// hooks.json) wins; else derive from `hook_event`. Applies the pending backstop.
pub fn derive_claude(status_arg: Option<&str>, hook_event: Option<&str>, msg: &str) -> Option<Update> {
    let status = match status_arg {
        Some(s) => Status::from_wire(s),
        None => status_from_event(hook_event?)?,
    };
    if status == Status::Pending {
        let m = msg.trim();
        if m.is_empty() || GENERIC_PENDING.contains(&m) {
            return None; // backstop: not a real "needs you"
        }
    }
    Some(Update { status, msg: msg.to_string() })
}

/// Pure: Codex only reports turn completion → `done`; anything else is a no-op.
pub fn derive_codex(event_type: &str, last_message: &str) -> Option<Update> {
    if event_type == "agent-turn-complete" {
        Some(Update { status: Status::Done, msg: last_message.to_string() })
    } else {
        None
    }
}

/// Terminal pane id from `$ZELLIJ_PANE_ID` (strip a `terminal_` prefix), or None
/// when not running under Zellij or the id is non-numeric.
fn pane_id_from_env() -> Option<u32> {
    if std::env::var_os("ZELLIJ").is_none() {
        return None;
    }
    let raw = std::env::var("ZELLIJ_PANE_ID").ok()?;
    raw.strip_prefix("terminal_").unwrap_or(&raw).parse::<u32>().ok()
}

fn git_repo_branch(cwd: &str) -> (String, String) {
    let top = Command::new("git").args(["-C", cwd, "rev-parse", "--show-toplevel"]).output().ok();
    let repo = top
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|p| p.rsplit('/').next().unwrap_or(&p).to_string())
        .unwrap_or_default();
    let branch = Command::new("git").args(["-C", cwd, "branch", "--show-current"]).output().ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    (repo, branch)
}

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

/// Thin IO wrapper: parse the agent input, derive, then broadcast. Never panics;
/// any failure is a silent no-op so the calling hook is never broken.
pub fn run(agent: &str, input: Option<&str>, status_arg: Option<&str>, dry_run: bool) {
    let Some(pane_id) = pane_id_from_env() else { return };

    let (update, cwd) = match agent {
        "claude" => {
            let raw = read_stdin();
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
            let event = v.get("hook_event_name").and_then(|x| x.as_str());
            let msg = v.get("message").and_then(|x| x.as_str())
                .or_else(|| v.get("last_assistant_message").and_then(|x| x.as_str()))
                .unwrap_or("");
            let cwd = v.get("cwd").and_then(|x| x.as_str()).map(str::to_string);
            (derive_claude(status_arg, event, msg), cwd)
        }
        "codex" => {
            let raw = input.unwrap_or("");
            let v: serde_json::Value = serde_json::from_str(raw).unwrap_or(serde_json::Value::Null);
            let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
            let msg = v.get("last-assistant-message").and_then(|x| x.as_str()).unwrap_or("");
            let cwd = v.get("cwd").and_then(|x| x.as_str()).map(str::to_string);
            (derive_codex(ty, msg), cwd)
        }
        _ => {
            eprintln!("zj-radar: unknown agent '{agent}' (expected: claude | codex)");
            return;
        }
    };

    let Some(update) = update else { return };
    let cwd = cwd
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("PWD").ok())
        .unwrap_or_else(|| ".".to_string());
    let (repo, branch) = git_repo_branch(&cwd);
    let on_focus = if update.status == Status::Done { Some(Status::Idle) } else { None };
    let payload = to_wire(pane_id, update.status, &repo, &branch, &update.msg, on_focus, agent);

    if dry_run {
        eprintln!("{payload}");
        return;
    }
    let _ = Command::new("zellij")
        .args(["pipe", "--name", "zj_radar.status.v1", "--", &payload])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Status;

    #[test]
    fn claude_explicit_status_passes_through() {
        let u = derive_claude(Some("running"), None, "anything").unwrap();
        assert_eq!(u.status, Status::Running);
    }

    #[test]
    fn claude_pending_with_real_message_is_kept() {
        let u = derive_claude(Some("pending"), None, "approve this?").unwrap();
        assert_eq!(u.status, Status::Pending);
        assert_eq!(u.msg, "approve this?");
    }

    #[test]
    fn claude_pending_backstop_drops_empty_and_generic() {
        assert!(derive_claude(Some("pending"), None, "").is_none());
        assert!(derive_claude(Some("pending"), None, "Claude needs attention").is_none());
        assert!(derive_claude(Some("pending"), None, "Claude Code needs your attention").is_none());
    }

    #[test]
    fn claude_derives_status_from_event_when_no_explicit_status() {
        assert_eq!(derive_claude(None, Some("UserPromptSubmit"), "").unwrap().status, Status::Running);
        assert_eq!(derive_claude(None, Some("PostToolUse"), "").unwrap().status, Status::Running);
        assert_eq!(derive_claude(None, Some("Stop"), "done").unwrap().status, Status::Done);
        assert!(derive_claude(None, Some("SomethingElse"), "").is_none());
    }

    #[test]
    fn codex_turn_complete_is_done_else_none() {
        assert_eq!(derive_codex("agent-turn-complete", "shipped it").unwrap().status, Status::Done);
        assert_eq!(derive_codex("agent-turn-complete", "shipped it").unwrap().msg, "shipped it");
        assert!(derive_codex("task-started", "").is_none());
    }
}
