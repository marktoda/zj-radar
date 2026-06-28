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
    std::env::var_os("ZELLIJ")?; // not under Zellij → no-op
    let raw = std::env::var("ZELLIJ_PANE_ID").ok()?;
    raw.strip_prefix("terminal_").unwrap_or(&raw).parse::<u32>().ok()
}

/// Derive the repository NAME from a git "common dir" path — the output of
/// `git rev-parse --git-common-dir` made absolute. The common dir always points
/// at the MAIN repo's git dir, even from inside a linked worktree, so this yields
/// the repo name (e.g. `pinky`) rather than the worktree's own directory name
/// (e.g. `reply-register`, which is what `--show-toplevel` returns in a worktree).
///
///   /Users/m/dev/pinky/.git        → "pinky"      (normal checkout or any worktree of it)
///   /Users/m/dev/pinky/.git/       → "pinky"
///   /Users/m/dev/acme.git          → "acme"       (bare repo)
///   .git                           → None         (relative — caller falls back)
fn repo_name_from_common_dir(common_dir: &str) -> Option<String> {
    let trimmed = common_dir.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if base == ".git" {
        // Repo root is the parent of the ".git" dir.
        let parent = trimmed[..trimmed.len() - base.len()].trim_end_matches('/');
        parent.rsplit('/').find(|s| !s.is_empty()).map(str::to_string)
    } else if let Some(stripped) = base.strip_suffix(".git") {
        // Bare repo "name.git".
        (!stripped.is_empty()).then(|| stripped.to_string())
    } else {
        // Unusual: a common dir not ending in .git — use its basename.
        Some(base.to_string())
    }
}

fn git_repo_branch(cwd: &str) -> (String, String) {
    // Resolve the repo name from the COMMON git dir so worktrees report the main
    // repo, not the worktree directory. Fall back to `--show-toplevel`'s basename
    // for git versions without `--path-format` (added in 2.31).
    let common = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output().ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .and_then(|d| repo_name_from_common_dir(&d));
    let repo = common.or_else(|| {
        Command::new("git").args(["-C", cwd, "rev-parse", "--show-toplevel"]).output().ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|p| p.rsplit('/').next().unwrap_or(&p).to_string())
    }).unwrap_or_default();
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
            let mut derived = derive_claude(status_arg, event, msg);
            // For PreToolUse/PostToolUse (running status), substitute the tool
            // activity string when available so the sidebar shows live action.
            if let Some(ref mut upd) = derived {
                if upd.status == Status::Running
                    && matches!(event, Some("PreToolUse") | Some("PostToolUse"))
                {
                    let tool_name = v.get("tool_name").and_then(|x| x.as_str()).unwrap_or("");
                    let tool_input = v.get("tool_input").unwrap_or(&serde_json::Value::Null);
                    if let Some(activity) = tool_activity(tool_name, tool_input) {
                        upd.msg = activity;
                    }
                }
            }
            (derived, cwd)
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

/// Short present-tense activity string for a tool invocation, or None if the
/// tool has no useful activity phrasing (caller keeps the prior msg).
pub fn tool_activity(tool_name: &str, tool_input: &serde_json::Value) -> Option<String> {
    match tool_name {
        "Edit" | "Write" | "MultiEdit" => {
            let path = tool_input.get("file_path")?.as_str()?;
            if path.is_empty() { return None; }
            let base = path.rsplit('/').next().unwrap_or(path);
            Some(format!("editing {base}"))
        }
        "NotebookEdit" => {
            let path = tool_input.get("notebook_path")?.as_str()?;
            if path.is_empty() { return None; }
            let base = path.rsplit('/').next().unwrap_or(path);
            Some(format!("editing {base}"))
        }
        "Read" => {
            let path = tool_input.get("file_path")?.as_str()?;
            if path.is_empty() { return None; }
            let base = path.rsplit('/').next().unwrap_or(path);
            Some(format!("reading {base}"))
        }
        "Grep" | "Glob" => Some("searching".to_string()),
        "WebFetch" | "WebSearch" => Some("searching web".to_string()),
        "Task" => Some("delegating".to_string()),
        "TodoWrite" => Some("planning".to_string()),
        "Bash" => {
            let cmd = tool_input.get("command")?.as_str()?;
            let cmd_lower = cmd.to_lowercase();
            if cmd.trim().is_empty() {
                return None;
            }
            if cmd_lower.contains("git push") {
                Some("pushing".to_string())
            } else if cmd_lower.contains("git commit") {
                Some("committing".to_string())
            } else if cmd_lower.contains("git pull") || cmd_lower.contains("git fetch") {
                Some("syncing".to_string())
            } else if cmd_lower.contains("test") {
                Some("running tests".to_string())
            } else if cmd_lower.contains("build") || cmd_lower.contains("compile") {
                Some("building".to_string())
            } else if cmd_lower.contains("install") {
                Some("installing".to_string())
            } else {
                let first_token = cmd.split_whitespace().next()?;
                let base = first_token.rsplit('/').next().unwrap_or(first_token);
                if base.is_empty() { return None; }
                Some(format!("running {base}"))
            }
        }
        _ => None,
    }
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

    // --- tool_activity tests ---

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn tool_edit_write_multiedit_reduce_to_basename() {
        for tool in &["Edit", "Write", "MultiEdit"] {
            let input = json(r#"{"file_path": "/path/to/auth.rs"}"#);
            assert_eq!(tool_activity(tool, &input).unwrap(), "editing auth.rs", "tool={tool}");
        }
    }

    #[test]
    fn tool_read_reduces_to_basename() {
        let input = json(r#"{"file_path": "/some/deep/path/mod.rs"}"#);
        assert_eq!(tool_activity("Read", &input).unwrap(), "reading mod.rs");
    }

    #[test]
    fn tool_notebook_edit_uses_notebook_path() {
        let input = json(r#"{"notebook_path": "/notebooks/analysis.ipynb"}"#);
        assert_eq!(tool_activity("NotebookEdit", &input).unwrap(), "editing analysis.ipynb");
    }

    #[test]
    fn tool_grep_and_glob_return_searching() {
        assert_eq!(tool_activity("Grep", &json(r#"{"pattern": "foo"}"#)).unwrap(), "searching");
        assert_eq!(tool_activity("Glob", &json(r#"{"pattern": "*.rs"}"#)).unwrap(), "searching");
    }

    #[test]
    fn tool_webfetch_and_websearch_return_searching_web() {
        assert_eq!(tool_activity("WebFetch", &json(r#"{"url": "https://example.com"}"#)).unwrap(), "searching web");
        assert_eq!(tool_activity("WebSearch", &json(r#"{"query": "rust async"}"#)).unwrap(), "searching web");
    }

    #[test]
    fn tool_task_returns_delegating() {
        assert_eq!(tool_activity("Task", &json(r#"{"description": "do X"}"#)).unwrap(), "delegating");
    }

    #[test]
    fn tool_todowrite_returns_planning() {
        assert_eq!(tool_activity("TodoWrite", &json(r#"{"todos": []}"#)).unwrap(), "planning");
    }

    #[test]
    fn tool_bash_git_push() {
        let input = json(r#"{"command": "git push origin main"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "pushing");
    }

    #[test]
    fn tool_bash_git_commit() {
        let input = json(r#"{"command": "git commit -m x"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "committing");
    }

    #[test]
    fn tool_bash_git_pull() {
        let input = json(r#"{"command": "git pull"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "syncing");
    }

    #[test]
    fn tool_bash_cargo_test() {
        let input = json(r#"{"command": "cargo test --features cli"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "running tests");
    }

    #[test]
    fn tool_bash_npm_run_build() {
        let input = json(r#"{"command": "npm run build"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "building");
    }

    #[test]
    fn tool_bash_pip_install() {
        let input = json(r#"{"command": "pip install foo"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "installing");
    }

    #[test]
    fn tool_bash_path_stripped_to_basename() {
        let input = json(r#"{"command": "/usr/bin/ls -la"}"#);
        assert_eq!(tool_activity("Bash", &input).unwrap(), "running ls");
    }

    #[test]
    fn tool_bash_empty_command_returns_none() {
        let input = json(r#"{"command": "   "}"#);
        assert!(tool_activity("Bash", &input).is_none());
    }

    #[test]
    fn tool_unknown_returns_none() {
        assert!(tool_activity("SomeFutureTool", &json("{}")).is_none());
    }

    #[test]
    fn tool_edit_missing_file_path_returns_none() {
        assert!(tool_activity("Edit", &json("{}")).is_none());
    }

    // --- repo_name_from_common_dir tests ---

    #[test]
    fn common_dir_normal_checkout_is_repo_name() {
        // A normal checkout's common dir is "<repo>/.git" → repo basename.
        assert_eq!(repo_name_from_common_dir("/Users/m/dev/pinky/.git"), Some("pinky".into()));
    }

    #[test]
    fn common_dir_worktree_resolves_to_main_repo() {
        // A worktree of "pinky" still reports the MAIN repo's common dir, so the
        // name is "pinky" — NOT the worktree dir "reply-register".
        assert_eq!(repo_name_from_common_dir("/Users/m/dev/pinky/.git"), Some("pinky".into()));
        // Trailing slash is tolerated.
        assert_eq!(repo_name_from_common_dir("/Users/m/dev/pinky/.git/"), Some("pinky".into()));
    }

    #[test]
    fn common_dir_bare_repo_strips_dot_git() {
        assert_eq!(repo_name_from_common_dir("/srv/git/acme.git"), Some("acme".into()));
    }

    #[test]
    fn common_dir_relative_or_empty_is_none() {
        // Relative ".git" has no resolvable parent → None (caller falls back).
        assert_eq!(repo_name_from_common_dir(".git"), None);
        assert_eq!(repo_name_from_common_dir(""), None);
        assert_eq!(repo_name_from_common_dir("   "), None);
    }
}
