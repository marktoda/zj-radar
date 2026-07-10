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

    Command::cargo_bin("zj-radar")
        .unwrap()
        .arg("notify")
        .arg("claude")
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_7")
        .write_stdin(hook)
        .assert()
        .success();

    let calls = shims.recorded("zellij");
    assert_eq!(calls.len(), 1, "expected exactly one zellij pipe broadcast");
    let c = &calls[0];

    // Payload is passed as argv after `--`, so join all recorded args to inspect.
    let argv = c.args.join(" ");
    assert!(
        c.args.contains(&"pipe".to_string()),
        "expected 'pipe' subcommand in: {argv}"
    );
    assert_eq!(c.stdin, "", "payload should be sent as argv, not stdin");
    assert!(
        argv.contains("\"pane\""),
        "payload missing pane field: {argv}"
    );
    assert!(
        argv.contains("\"id\":7"),
        "payload missing derived pane id 7 (ZELLIJ_PANE_ID=terminal_7): {argv}"
    );
    assert!(
        argv.contains("editing auth.rs"),
        "payload missing activity string: {argv}"
    );
}

#[test]
fn claude_posttooluse_bash_git_push_broadcasts_pushing() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");

    let hook = r#"{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#;

    Command::cargo_bin("zj-radar")
        .unwrap()
        .arg("notify")
        .arg("claude")
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_7")
        .write_stdin(hook)
        .assert()
        .success();

    let calls = shims.recorded("zellij");
    assert_eq!(calls.len(), 1, "expected exactly one zellij pipe broadcast");
    let argv = calls[0].args.join(" ");
    assert!(
        argv.contains("pushing"),
        "payload missing 'pushing' activity: {argv}"
    );
}

#[test]
fn no_zellij_env_exits_clean_without_broadcast() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    Command::cargo_bin("zj-radar")
        .unwrap()
        .arg("notify")
        .arg("claude")
        .env("PATH", shims.path_env())
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_PANE_ID")
        .write_stdin(r#"{"hook_event_name":"Stop","cwd":"/tmp"}"#)
        .assert()
        .success();
    assert!(
        shims.recorded("zellij").is_empty(),
        "must not broadcast outside Zellij"
    );
}

#[test]
fn hung_zellij_pipe_is_killed_at_the_send_deadline() {
    // A rail instance wedged at Zellij's permission prompt blocks `zellij pipe`
    // forever (CLI-pipe backpressure: the client is held until every plugin
    // consumes the message). Hooks fire per tool call, so an unbounded send
    // leaks one blocked client + two server FDs per call until the Zellij
    // server EMFILEs and the session crashes. The producer must cap the wait
    // and reap the child; the message itself is already queued server-side,
    // so killing the client loses nothing.
    let shims = ShimDir::new();
    shims.add_hanging_recorder("zellij", 60);
    shims.add_fake_git("/home/u/myrepo", "main");

    let hook = r#"{"hook_event_name":"Stop","cwd":"/home/u/myrepo"}"#;

    let start = std::time::Instant::now();
    Command::cargo_bin("zj-radar")
        .unwrap()
        .arg("notify")
        .arg("claude")
        .arg("--status")
        .arg("done")
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_7")
        // 3s, not 1: the shim must exec and write its log line BEFORE the
        // deadline kill, or the recorded-broadcast assertion below races the
        // reaper. Under full-parallel test load (nix check builds) a 1s
        // deadline lost that race; 3s keeps the property (return at the
        // deadline, not at the 60s hang) with real scheduling headroom.
        .env("ZJ_RADAR_PIPE_TIMEOUT", "3")
        .timeout(std::time::Duration::from_secs(15))
        .write_stdin(hook)
        .assert()
        .success();
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "notify must return at the send deadline, not ride a wedged pipe ({}s)",
        start.elapsed().as_secs()
    );
    // The broadcast was still attempted (payload handed to zellij pre-hang).
    assert_eq!(shims.recorded("zellij").len(), 1);
}

#[test]
fn hung_pipe_is_reaped_even_when_notify_itself_is_killed_mid_send() {
    // The deadline in `broadcast`'s parent loop only helps while the producer
    // LIVES to enforce it — and hook runners kill their hooks. A SIGKILLed
    // notify must not orphan its blocked `zellij pipe` client: each orphan
    // pins two Zellij-server FDs forever, and at hook rate that is the EMFILE
    // session-crash class (observed in production as orphaned clients minutes
    // old, ppid 1). The spawned subtree carries its own watchdog
    // (`core::pipe::self_limiting_pipe_argv`); killing the producer must not
    // disarm it.
    let shims = ShimDir::new();
    shims.add_hanging_recorder_reporting_pid("zellij", 60);
    shims.add_fake_git("/home/u/myrepo", "main");

    let mut notify = std::process::Command::new(env!("CARGO_BIN_EXE_zj-radar"))
        .args(["notify", "claude", "--status", "done"])
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_7")
        .env("ZJ_RADAR_PIPE_TIMEOUT", "2")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = notify.stdin.take().unwrap();
        stdin
            .write_all(br#"{"hook_event_name":"Stop","cwd":"/home/u/myrepo"}"#)
            .unwrap();
    } // scope end closes stdin so the adapter's read returns

    // Wait until the client is hung, then kill the producer BEFORE its 2s
    // deadline — the moment a real hook runner would.
    let pid = shims.wait_for_hung_pid("zellij", std::time::Duration::from_secs(10));
    notify.kill().unwrap();
    notify.wait().unwrap();

    // The orphaned subtree must still reap the hung client at its own
    // deadline. Poll with slack for loaded CI rather than sleeping once.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !alive {
            break; // reaped — the leak is closed
        }
        if std::time::Instant::now() >= deadline {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
            panic!("blocked `zellij pipe` client leaked past its watchdog after the producer died");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
