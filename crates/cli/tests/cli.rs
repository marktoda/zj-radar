use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zj-radar"))
}

#[test]
fn setup_codex_installs_hooks_without_touching_foreign_notify() {
    let codex_home = TempDir::new().unwrap();
    let config = codex_home.path().join("config.toml");
    fs::write(&config, "notify = [\"/other/notifier\", \"turn-ended\"]\n").unwrap();

    let output = Command::new(cli_bin())
        .args(["setup", "codex", "--yes"])
        .env("CODEX_HOME", codex_home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let config_after = fs::read_to_string(config).unwrap();
    assert_eq!(
        config_after,
        "notify = [\"/other/notifier\", \"turn-ended\"]\n"
    );
    let hooks = fs::read_to_string(codex_home.path().join("hooks.json")).unwrap();
    assert!(hooks.contains("ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex"));
    assert!(hooks.contains("\"PermissionRequest\""));
    assert!(hooks.contains("\"Stop\""));
}

#[cfg(unix)]
#[test]
fn notify_codex_hook_broadcasts_pending_payload() {
    use std::os::unix::fs::PermissionsExt;

    let bin_dir = TempDir::new().unwrap();
    let capture = bin_dir.path().join("zellij-args.txt");
    let fake_zellij = bin_dir.path().join("zellij");
    fs::write(
        &fake_zellij,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ZJ_RADAR_CAPTURE\"\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&fake_zellij).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&fake_zellij, perms).unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    let mut child = Command::new(cli_bin())
        .args(["notify", "codex"])
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_42")
        .env("ZJ_RADAR_CAPTURE", &capture)
        .env("PATH", format!("{}:{old_path}", bin_dir.path().display()))
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(
            br#"{
              "hook_event_name": "PermissionRequest",
              "cwd": ".",
              "tool_name": "Bash",
              "tool_input": {
                "command": "git push",
                "description": "Approve network access?"
              }
            }"#,
        )
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());

    let captured = fs::read_to_string(capture).unwrap();
    assert!(captured.contains("pipe\n"));
    assert!(captured.contains("--name\nzj_radar.status.v1\n"));
    let payload = captured.lines().last().unwrap();
    let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert_eq!(payload["source"], "codex");
    assert_eq!(payload["status"], "pending");
    assert_eq!(payload["pane"]["id"], 42);
    assert_eq!(payload["msg"], "Approve network access?");
}
