//! `zj-radar notify <agent>` — derive status, build payload, broadcast.

use super::codex;
use super::events::{tool_activity, Update};
use crate::payload::to_wire;
use crate::status::Status;
use std::io::Read;
use std::process::Command;

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
pub fn derive_claude(
    status_arg: Option<&str>,
    hook_event: Option<&str>,
    msg: &str,
) -> Option<Update> {
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
    // A running broadcast with no message would render as a blank active row.
    // Give it a neutral baseline; run()'s tool-activity substitution refines it
    // when a tool name/input is present.
    let msg = if status == Status::Running && msg.trim().is_empty() {
        "working".to_string()
    } else {
        msg.to_string()
    };
    Some(Update { status, msg })
}

/// Terminal pane id from `$ZELLIJ_PANE_ID` (strip a `terminal_` prefix), or None
/// when not running under Zellij or the id is non-numeric.
fn pane_id_from_env() -> Option<u32> {
    std::env::var_os("ZELLIJ")?; // not under Zellij → no-op
    let raw = std::env::var("ZELLIJ_PANE_ID").ok()?;
    raw.strip_prefix("terminal_")
        .unwrap_or(&raw)
        .parse::<u32>()
        .ok()
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
        parent
            .rsplit('/')
            .find(|s| !s.is_empty())
            .map(str::to_string)
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
        .args([
            "-C",
            cwd,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .and_then(|d| repo_name_from_common_dir(&d));
    let repo = common
        .or_else(|| {
            Command::new("git")
                .args(["-C", cwd, "rev-parse", "--show-toplevel"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
                .map(|p| p.rsplit('/').next().unwrap_or(&p).to_string())
        })
        .unwrap_or_default();
    let branch = Command::new("git")
        .args(["-C", cwd, "branch", "--show-current"])
        .output()
        .ok()
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
    let Some(pane_id) = pane_id_from_env() else {
        return;
    };

    let (update, cwd) = match agent {
        "claude" => {
            let raw = read_stdin();
            let v: serde_json::Value =
                serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
            let event = v.get("hook_event_name").and_then(|x| x.as_str());
            let msg = v
                .get("message")
                .and_then(|x| x.as_str())
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
            let raw = input.map(str::to_string).unwrap_or_else(read_stdin);
            match codex::derive_update(&raw) {
                Some((update, cwd)) => (Some(update), cwd),
                None => (None, None),
            }
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
    let on_focus = if update.status == Status::Done {
        Some(Status::Idle)
    } else {
        None
    };
    let payload = to_wire(
        pane_id,
        update.status,
        &repo,
        &branch,
        &update.msg,
        on_focus,
        agent,
    );

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
    fn claude_running_with_empty_msg_falls_back_to_working() {
        // A running broadcast with no activity text must not render as a blank
        // active row (the bare `◐ ✳` line) — derive a neutral "working"
        // baseline. run()'s tool-activity substitution still refines it when a
        // tool name/input is present.
        let u = derive_claude(Some("running"), None, "").unwrap();
        assert_eq!(u.status, Status::Running);
        assert_eq!(u.msg, "working");
        // Whitespace-only is also empty.
        assert_eq!(derive_claude(Some("running"), None, "   ").unwrap().msg, "working");
        // Event-derived running (no explicit status) with no message too.
        assert_eq!(
            derive_claude(None, Some("UserPromptSubmit"), "").unwrap().msg,
            "working"
        );
    }

    #[test]
    fn claude_running_with_real_msg_is_unchanged() {
        let u = derive_claude(Some("running"), None, "compiling").unwrap();
        assert_eq!(u.msg, "compiling");
    }

    #[test]
    fn claude_derives_status_from_event_when_no_explicit_status() {
        assert_eq!(
            derive_claude(None, Some("UserPromptSubmit"), "")
                .unwrap()
                .status,
            Status::Running
        );
        assert_eq!(
            derive_claude(None, Some("PostToolUse"), "").unwrap().status,
            Status::Running
        );
        assert_eq!(
            derive_claude(None, Some("Stop"), "done").unwrap().status,
            Status::Done
        );
        assert!(derive_claude(None, Some("SomethingElse"), "").is_none());
    }

    // --- repo_name_from_common_dir tests ---

    #[test]
    fn common_dir_normal_checkout_is_repo_name() {
        // A normal checkout's common dir is "<repo>/.git" → repo basename.
        assert_eq!(
            repo_name_from_common_dir("/Users/m/dev/pinky/.git"),
            Some("pinky".into())
        );
    }

    #[test]
    fn common_dir_worktree_resolves_to_main_repo() {
        // A worktree of "pinky" still reports the MAIN repo's common dir, so the
        // name is "pinky" — NOT the worktree dir "reply-register".
        assert_eq!(
            repo_name_from_common_dir("/Users/m/dev/pinky/.git"),
            Some("pinky".into())
        );
        // Trailing slash is tolerated.
        assert_eq!(
            repo_name_from_common_dir("/Users/m/dev/pinky/.git/"),
            Some("pinky".into())
        );
    }

    #[test]
    fn common_dir_bare_repo_strips_dot_git() {
        assert_eq!(
            repo_name_from_common_dir("/srv/git/acme.git"),
            Some("acme".into())
        );
    }

    #[test]
    fn common_dir_relative_or_empty_is_none() {
        // Relative ".git" has no resolvable parent → None (caller falls back).
        assert_eq!(repo_name_from_common_dir(".git"), None);
        assert_eq!(repo_name_from_common_dir(""), None);
        assert_eq!(repo_name_from_common_dir("   "), None);
    }
}
