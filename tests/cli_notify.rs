mod support;
use assert_cmd::Command;
use support::ShimDir;

// The CLI passes the payload via argv:
//   zellij pipe --name zj_radar.status.v1 -- <json>
// The recorder captures all argv in `args` (split by whitespace); stdin is empty.
// Since the JSON payload may contain spaces, we join args back and search the full string.

#[test]
fn claude_posttooluse_edit_broadcasts_editing_activity() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");

    let hook = r#"{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Edit","tool_input":{"file_path":"/home/u/myrepo/src/auth.rs"}}"#;

    Command::cargo_bin("zj-radar").unwrap()
        .arg("notify").arg("claude")
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_7")
        .write_stdin(hook)
        .assert().success();

    let calls = shims.recorded("zellij");
    assert_eq!(calls.len(), 1, "expected exactly one zellij pipe broadcast");
    let c = &calls[0];

    // Payload is passed as argv after `--`, so join all recorded args to inspect.
    let argv = c.args.join(" ");
    assert!(c.args.contains(&"pipe".to_string()), "expected 'pipe' subcommand in: {argv}");
    assert!(argv.contains("\"pane\""), "payload missing pane field: {argv}");
    assert!(argv.contains("\"id\":7"), "payload missing derived pane id 7 (ZELLIJ_PANE_ID=terminal_7): {argv}");
    assert!(argv.contains("editing auth.rs"), "payload missing activity string: {argv}");
}

#[test]
fn claude_posttooluse_bash_git_push_broadcasts_pushing() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");

    let hook = r#"{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#;

    Command::cargo_bin("zj-radar").unwrap()
        .arg("notify").arg("claude")
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_7")
        .write_stdin(hook)
        .assert().success();

    let calls = shims.recorded("zellij");
    assert_eq!(calls.len(), 1, "expected exactly one zellij pipe broadcast");
    let argv = calls[0].args.join(" ");
    assert!(argv.contains("pushing"), "payload missing 'pushing' activity: {argv}");
}

#[test]
fn no_zellij_env_exits_clean_without_broadcast() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    Command::cargo_bin("zj-radar").unwrap()
        .arg("notify").arg("claude")
        .env("PATH", shims.path_env())
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_PANE_ID")
        .write_stdin(r#"{"hook_event_name":"Stop","cwd":"/tmp"}"#)
        .assert().success();
    assert!(shims.recorded("zellij").is_empty(), "must not broadcast outside Zellij");
}
