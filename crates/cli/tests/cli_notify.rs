mod support;
use assert_cmd::Command;
use support::ShimDir;

// The CLI passes the payload via argv:
//   zellij pipe --name zj_radar.status.v1 -- <json>
// The recorder captures all argv in `args` (split by whitespace); stdin is empty.
// Since the JSON payload may contain spaces, `ShimDir::sole_pipe_argv` joins the
// args back into one string the assertions search.

// Every invocation pins the last-sent dedup state dir inside the shim tempdir
// (`TMPDIR`; `XDG_RUNTIME_DIR` would win on a Linux desktop) and drops the
// developer's own `ZELLIJ_SESSION_NAME`: `cargo test` from inside a Zellij
// pane would otherwise share one real record between parallel tests — and
// dedup the very sends these tests count. Tests that WANT dedup armed set a
// session name of their own (`notify_deduped`).

/// Run `zj-radar notify <agent>` under the shims with `hook` piped to stdin,
/// pane id 7 — the shared invocation every broadcast test starts from.
fn notify(shims: &ShimDir, agent: &str, hook: &str) {
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["notify", agent])
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_7")
        .env("TMPDIR", shims.dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("ZELLIJ_SESSION_NAME")
        .write_stdin(hook)
        .assert()
        .success();
}

/// `notify claude --status <status>` with the last-sent dedup ARMED: a session
/// identity (dedup is keyed per session × pane) and the state dir pinned
/// inside the shim tempdir (`TMPDIR`, with `XDG_RUNTIME_DIR` — which would
/// win on a Linux desktop — removed) so tests never touch the real one.
fn notify_deduped(shims: &ShimDir, status: &str, hook: &str, extra_env: &[(&str, &str)]) {
    let mut cmd = Command::cargo_bin("zj-radar").unwrap();
    cmd.args(["notify", "claude", "--status", status])
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_7")
        .env("ZELLIJ_SESSION_NAME", "dedup-test")
        .env("TMPDIR", shims.dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("ZJ_RADAR_NO_DEDUP");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.write_stdin(hook).assert().success();
}

const PRE_EDIT: &str = r#"{"hook_event_name":"PreToolUse","cwd":"/home/u/myrepo","tool_name":"Edit","tool_input":{"file_path":"/home/u/myrepo/src/auth.rs"}}"#;
const POST_EDIT: &str = r#"{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Edit","tool_input":{"file_path":"/home/u/myrepo/src/auth.rs"}}"#;

#[test]
fn identical_pre_and_post_running_payloads_send_once() {
    // PreToolUse and PostToolUse derive byte-identical wire payloads; the
    // second is a duplicate the rail would no-op anyway, so the producer
    // drops it before spending a git probe or a `zellij pipe` fan-out.
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");

    notify_deduped(&shims, "running", PRE_EDIT, &[]);
    notify_deduped(&shims, "running", POST_EDIT, &[]);
    assert_eq!(shims.recorded("zellij").len(), 1, "the Post repeat must be deduped");

    // A different activity is a different payload — sent.
    let read = r#"{"hook_event_name":"PreToolUse","cwd":"/home/u/myrepo","tool_name":"Read","tool_input":{"file_path":"/home/u/myrepo/README.md"}}"#;
    notify_deduped(&shims, "running", read, &[]);
    assert_eq!(shims.recorded("zellij").len(), 2);
}

#[test]
fn pending_edge_passes_and_the_recovery_running_is_sent() {
    // The Pending→Running recovery edge (the reason PostToolUse stays
    // registered): Notification→pending is never skipped and overwrites the
    // record, so the PostToolUse→running that follows differs and goes out —
    // an answered permission prompt must clear "needs you" at once.
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");

    let pending = r#"{"hook_event_name":"Notification","cwd":"/home/u/myrepo","message":"Claude needs your permission to use Edit"}"#;
    notify_deduped(&shims, "running", PRE_EDIT, &[]);
    notify_deduped(&shims, "pending", pending, &[]);
    notify_deduped(&shims, "running", POST_EDIT, &[]);
    let sent: Vec<String> = shims
        .recorded("zellij")
        .iter()
        .map(|c| c.args.join(" "))
        .collect();
    assert_eq!(sent.len(), 3, "running, pending, recovery running: {sent:?}");
    assert!(sent[1].contains("\"status\":\"pending\""), "{sent:?}");
    assert!(sent[2].contains("\"status\":\"running\""), "{sent:?}");
    // …and only the recovery's own repeat collapses.
    notify_deduped(&shims, "running", POST_EDIT, &[]);
    assert_eq!(shims.recorded("zellij").len(), 3);
}

#[test]
fn edges_are_never_deduped() {
    // Two identical `done` sends both go out: a dropped edge loses real state
    // (the rail may have cleared the row in between), only running repeats.
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");
    let stop = r#"{"hook_event_name":"Stop","cwd":"/home/u/myrepo","last_assistant_message":"finished"}"#;
    notify_deduped(&shims, "done", stop, &[]);
    notify_deduped(&shims, "done", stop, &[]);
    assert_eq!(shims.recorded("zellij").len(), 2);
}

#[test]
fn failed_send_is_not_recorded_so_the_repeat_is_retried() {
    // The sh wrapper exits with `zellij pipe`'s status; a failed client (no
    // server, killed at the deadline) must not be recorded as delivered, or
    // the pane would suppress its own retry for the dedup TTL.
    let shims = ShimDir::new();
    shims.add_recorder_exiting("zellij", 1);
    shims.add_fake_git("/home/u/myrepo", "main");
    notify_deduped(&shims, "running", PRE_EDIT, &[]);
    notify_deduped(&shims, "running", POST_EDIT, &[]);
    assert_eq!(shims.recorded("zellij").len(), 2, "an unconfirmed send must be retried");
}

#[test]
fn no_dedup_env_disables_the_skip() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");
    notify_deduped(&shims, "running", PRE_EDIT, &[("ZJ_RADAR_NO_DEDUP", "1")]);
    notify_deduped(&shims, "running", POST_EDIT, &[("ZJ_RADAR_NO_DEDUP", "1")]);
    assert_eq!(shims.recorded("zellij").len(), 2);
}

#[test]
fn dedup_state_is_scoped_per_pane_and_session() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");
    notify_deduped(&shims, "running", PRE_EDIT, &[]);
    // Same payload from another pane: its own record, so it is sent.
    notify_deduped(&shims, "running", POST_EDIT, &[("ZELLIJ_PANE_ID", "terminal_8")]);
    // Same pane id in another session: also its own record.
    notify_deduped(&shims, "running", POST_EDIT, &[("ZELLIJ_SESSION_NAME", "other")]);
    assert_eq!(shims.recorded("zellij").len(), 3);
    // The state files landed under the injected TMPDIR, not the real one.
    let state = shims.dir.path().join("zj-radar");
    let mut names: Vec<_> = std::fs::read_dir(&state)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "last-sent.dedup-test.7.json",
            "last-sent.dedup-test.8.json",
            "last-sent.other.7.json"
        ]
    );
}

#[test]
fn claude_posttooluse_edit_broadcasts_editing_activity() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");

    let hook = r#"{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Edit","tool_input":{"file_path":"/home/u/myrepo/src/auth.rs"}}"#;
    notify(&shims, "claude", hook);

    let argv = shims.sole_pipe_argv();
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
    // The hook's cwd does not exist on this machine, so the native .git walk
    // declines and repo/branch come from the git fallback — the fake here.
    // Pins that the spawn path stays wired behind the native one.
    assert!(
        argv.contains("\"repo\":\"myrepo\"") && argv.contains("\"branch\":\"main\""),
        "repo/branch must come from the git fallback for an unresolvable cwd: {argv}"
    );
}

#[test]
fn claude_posttooluse_bash_git_push_broadcasts_pushing() {
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");

    let hook = r#"{"hook_event_name":"PostToolUse","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#;
    notify(&shims, "claude", hook);

    let argv = shims.sole_pipe_argv();
    assert!(
        argv.contains("pushing"),
        "payload missing 'pushing' activity: {argv}"
    );
}

#[test]
fn codex_permissionrequest_broadcasts_pending_payload() {
    // The codex twin of the claude cases above: a PermissionRequest hook must
    // broadcast a pending payload naming the question, on the status pipe.
    let shims = ShimDir::new();
    shims.add_recorder("zellij");
    shims.add_fake_git("/home/u/myrepo", "main");

    let hook = r#"{"hook_event_name":"PermissionRequest","cwd":"/home/u/myrepo","tool_name":"Bash","tool_input":{"command":"git push","description":"Approve network access?"}}"#;
    notify(&shims, "codex", hook);

    let argv = shims.sole_pipe_argv();
    assert!(
        argv.contains("--name zj_radar.status.v1"),
        "broadcast must target the status pipe: {argv}"
    );
    assert!(argv.contains("\"source\":\"codex\""), "payload: {argv}");
    assert!(argv.contains("\"status\":\"pending\""), "payload: {argv}");
    assert!(
        argv.contains("\"id\":7"),
        "payload missing derived pane id 7 (ZELLIJ_PANE_ID=terminal_7): {argv}"
    );
    assert!(argv.contains("Approve network access?"), "payload: {argv}");
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
        .env("TMPDIR", shims.dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("ZELLIJ_SESSION_NAME")
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
        .env("TMPDIR", shims.dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("ZELLIJ_SESSION_NAME")
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
    shims.add_hanging_recorder("zellij", 60);
    shims.add_fake_git("/home/u/myrepo", "main");

    let started = std::time::Instant::now();
    let mut notify = std::process::Command::new(env!("CARGO_BIN_EXE_zj-radar"))
        .args(["notify", "claude", "--status", "done"])
        .env("PATH", shims.path_env())
        .env("ZELLIJ", "1")
        .env("ZELLIJ_PANE_ID", "terminal_7")
        .env("TMPDIR", shims.dir.path())
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("ZELLIJ_SESSION_NAME")
        // Generous: the guard below invalidates the test if the kill cannot
        // land before this deadline, and a workspace-wide `cargo test` on a
        // loaded machine took >4 s just to reach the hung client.
        .env("ZJ_RADAR_PIPE_TIMEOUT", "8")
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

    // Wait until the client is hung, then kill the producer BEFORE its 4s
    // deadline — the moment a real hook runner would. The elapsed guard is
    // what makes this test trustworthy: past the deadline a still-living
    // notify reaps the client itself (the pre-fix behavior), and a green
    // result would silently re-test the ordinary hung path above instead of
    // the producer-death orphan this test exists to catch. Better a loud
    // too-slow failure than a false pass.
    let pid = shims.wait_for_hung_pid("zellij", std::time::Duration::from_secs(10));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(8),
        "test invalidated, not failed: machine too loaded to reach the kill \
         before notify's own send deadline — the producer-death regression \
         would be unobservable ({}ms elapsed)",
        started.elapsed().as_millis()
    );
    notify.kill().unwrap();
    notify.wait().unwrap();

    // The orphaned subtree must still reap the hung client at its own
    // deadline (up to 8 s after the send began). Poll with slack for loaded
    // CI rather than sleeping once.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
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
    // Reaped — and the broadcast was still attempted: the payload reached
    // zellij's argv before the hang, so killing the client lost nothing.
    assert_eq!(shims.recorded("zellij").len(), 1);
}
