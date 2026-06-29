//! `zj-radar notify <agent>` — the host shell: read the hook payload, derive an
//! update behind the agent-intake seam, resolve repo/branch, broadcast.
//! All agent-specific decisions live in `agents/`; this file is agent-agnostic
//! plumbing plus the genuinely host-bound helpers (env, git, stdin).

use super::agents::{Agent, Intake};
use crate::payload::to_wire;
use crate::status::Status;
use std::io::Read;
use std::process::Command;

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

/// Thin IO wrapper: source the payload, derive behind the agent seam, then
/// broadcast. Never panics; any failure is a silent no-op so the calling hook is
/// never broken.
pub fn run(agent: &str, input: Option<&str>, status_arg: Option<&str>, dry_run: bool) {
    let Some(pane_id) = pane_id_from_env() else {
        return;
    };
    let Some(agent) = Agent::from_cli(agent) else {
        eprintln!("zj-radar: unknown agent '{agent}' (expected: claude | codex)");
        return;
    };

    // Uniform input sourcing: argv `input` if present (Codex's legacy notify),
    // else stdin (Claude and modern Codex hooks). The adapter parses it.
    let raw = input.map(str::to_owned).unwrap_or_else(read_stdin);
    let Some(update) = agent.derive(&Intake {
        raw: &raw,
        status_arg,
    }) else {
        return;
    };

    let cwd = update
        .cwd
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
        agent.source(),
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
