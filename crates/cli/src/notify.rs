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

/// Run `git -C <cwd> <args…>` and return its trimmed stdout, or `None` when git
/// can't be spawned, exits non-zero, or produces only whitespace. The single
/// shape behind every git probe here, so the success/trim/empty handling can't
/// drift across calls (and treating empty output as "absent" matches what every
/// caller wanted: two filtered it explicitly, the third used `unwrap_or_default`).
fn git_output(cwd: &str, args: &[&str]) -> Option<String> {
    let trimmed = Command::new("git")
        .args(["-C", cwd])
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    (!trimmed.is_empty()).then_some(trimmed)
}

fn git_repo_branch(cwd: &str) -> (String, String) {
    // Resolve the repo name from the COMMON git dir so worktrees report the main
    // repo, not the worktree directory. Fall back to `--show-toplevel`'s basename
    // for git versions without `--path-format` (added in 2.31).
    let repo = git_output(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .and_then(|d| repo_name_from_common_dir(&d))
    .or_else(|| {
        git_output(cwd, &["rev-parse", "--show-toplevel"])
            .map(|p| p.rsplit('/').next().unwrap_or(&p).to_string())
    })
    .unwrap_or_default();
    let branch = git_output(cwd, &["branch", "--show-current"]).unwrap_or_default();
    (repo, branch)
}

/// Bytes of stdin we're willing to buffer. Generous — 8 MiB dwarfs any real hook
/// payload (even a Write tool's full file `content`) — so it never truncates a
/// legitimate input; it only bounds a degenerate multi-MB/GB stream. Note this is
/// the *input* cap, distinct from the plugin's 64 KB *wire* cap on the broadcast
/// payload the CLI produces.
const MAX_STDIN_BYTES: u64 = 8 << 20;

fn read_stdin() -> String {
    read_capped(std::io::stdin().lock(), MAX_STDIN_BYTES)
}

/// Read up to `cap` bytes as UTF-8, ignoring IO/UTF-8 errors (the caller derives
/// from whatever parses; a truncated or non-UTF-8 payload just fails to parse and
/// no-ops — the safe degradation for a fire-and-forget hook). Split out so the
/// bound is unit-tested without a real stdin.
fn read_capped<R: Read>(reader: R, cap: u64) -> String {
    let mut s = String::new();
    let _ = reader.take(cap).read_to_string(&mut s);
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
    // Bound the message client-side so a pathologically long final assistant
    // message can't push the whole payload past the plugin's MAX_PAYLOAD_BYTES
    // cap — which would drop the *entire* status update (e.g. losing a `done`
    // edge and leaving the tab stuck "working"). The cap is generous relative to
    // the plugin's 60-char display cap so its sanitizer still has content after
    // control-char stripping.
    let msg: String = update.msg.chars().take(512).collect();
    let payload = to_wire(
        pane_id,
        update.status,
        &repo,
        &branch,
        &msg,
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

    // --- bounded stdin read ---

    #[test]
    fn read_capped_reads_small_input_whole() {
        assert_eq!(read_capped(std::io::Cursor::new(b"hello".to_vec()), 1024), "hello");
        assert_eq!(read_capped(std::io::Cursor::new(Vec::new()), 1024), "");
    }

    #[test]
    fn read_capped_bounds_oversized_input() {
        // A stream larger than the cap is truncated to the cap, never buffered
        // whole — the guard against a pathological producer.
        let big = vec![b'x'; 10_000];
        assert_eq!(read_capped(std::io::Cursor::new(big), 64).len(), 64);
    }
}
