//! End-to-end smoke tests for the two halves of the CLI: `setup codex` writes
//! hooks.json without touching a foreign notify slot, and `notify codex`
//! broadcasts a real `zellij pipe` payload (captured via the shared shim).

mod support;

use assert_cmd::Command;
use std::fs;
use support::{ShimDir, HOOK_MARKER};
use tempfile::TempDir;

#[test]
fn setup_codex_installs_hooks_without_touching_foreign_notify() {
    let codex_home = TempDir::new().unwrap();
    let config = codex_home.path().join("config.toml");
    fs::write(&config, "notify = [\"/other/notifier\", \"turn-ended\"]\n").unwrap();

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "codex", "--yes"])
        .env("CODEX_HOME", codex_home.path())
        .assert()
        .success();

    let config_after = fs::read_to_string(config).unwrap();
    assert_eq!(
        config_after,
        "notify = [\"/other/notifier\", \"turn-ended\"]\n"
    );
    let hooks = fs::read_to_string(codex_home.path().join("hooks.json")).unwrap();
    assert!(hooks.contains(HOOK_MARKER));
    assert!(hooks.contains("\"PermissionRequest\""));
    assert!(hooks.contains("\"Stop\""));
}

#[cfg(unix)]
#[test]
fn notify_codex_hook_broadcasts_pending_payload() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");

    let hook = r#"{"hook_event_name":"PermissionRequest","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git push","description":"Approve network access?"}}"#;

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["notify", "codex"])
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_42")
        .write_stdin(hook)
        .assert()
        .success();

    let calls = shims.recorded("zellij");
    assert_eq!(calls.len(), 1, "expected exactly one zellij pipe broadcast");
    let c = &calls[0];
    assert!(
        c.args.contains(&"pipe".to_string()),
        "expected the pipe subcommand in: {:?}",
        c.args
    );
    // Payload rides argv after `--`; join the recorded args to inspect it
    // (spaces inside the JSON survive the shim's whitespace split + rejoin).
    let argv = c.args.join(" ");
    assert!(
        argv.contains("--name zj_radar.status.v1"),
        "broadcast must target the status pipe: {argv}"
    );
    assert!(argv.contains("\"source\":\"codex\""), "payload: {argv}");
    assert!(argv.contains("\"status\":\"pending\""), "payload: {argv}");
    assert!(
        argv.contains("\"id\":42"),
        "payload missing derived pane id 42 (ZELLIJ_PANE_ID=terminal_42): {argv}"
    );
    assert!(argv.contains("Approve network access?"), "payload: {argv}");
}
