//! The bounded-send argv for the status broadcast — how every producer should
//! actually spawn `zellij pipe`.
//!
//! `zellij pipe` is a backpressure channel: the client process is held until
//! **every** loaded plugin instance consumes the message, and an instance
//! wedged at Zellij's permission prompt holds it forever. Producers fire at
//! hook rate (one send per tool call), and each blocked client pins two FDs in
//! the Zellij *server*, so an unbounded send turns one wedged rail into an
//! EMFILE crash of the whole session. Hence the send deadline.
//!
//! The deadline cannot live only in the producer process: hook runners kill
//! their hooks, and a producer killed mid-send never runs its kill-on-deadline
//! — the blocked `zellij pipe` child re-parents to init and leaks forever
//! (observed in production as orphaned clients pinning server FDs for hours).
//! This argv makes the spawned subtree limit **itself**: a detached sleep+kill
//! watchdog rides inside the same `sh`, so the hung client is reaped no matter
//! what happens to the process that spawned it. Killing a client past the
//! deadline retracts nothing — the message is queued server-side the moment it
//! is sent — so latest-wins ordering holds.

use crate::payload::STATUS_PIPE_NAME;

/// Default send deadline in whole seconds — orders of magnitude above a
/// healthy send (milliseconds) yet caps a wedged one at hook rate. The CLI's
/// `ZJ_RADAR_PIPE_TIMEOUT` override falls back to this; the plugin (which has
/// no environment) uses it directly.
pub const DEFAULT_PIPE_TIMEOUT_SECS: u64 = 5;

// $1 = deadline seconds, $2 = pipe name, $3 = payload — positional parameters,
// never interpolated into the script (same no-escaping rule as the plugin's
// `notify_command`), so an arbitrary payload cannot break out of the command.
//
// Accepted residuals, shared with notify.sh's inline copy of this idiom (keep
// the two in sync):
//  - The watchdog's expiring `kill` could in principle target a recycled pid;
//    the disarm after a normal send narrows that window to microseconds.
//  - Disarming kills only the watchdog subshell: its orphaned `sleep` lingers
//    ≤ $1 seconds after every healthy send and exits without acting (the kill
//    line is unreachable once the subshell is gone) — accepted `ps` noise.
//  - The kill is a single SIGTERM to the direct child: sufficient because
//    `zellij pipe` is one process with the default TERM disposition (it execs,
//    forks no helpers, traps nothing). A client that ignored TERM or hid the
//    blocked process behind a child would outlive the watchdog.
//  - If the watchdog's own fork fails (process-table exhaustion), the client
//    runs unbounded: forks 1-2 succeeding while fork 3 fails is a double-
//    failure corner. The total fix — spawning the subtree in its own process
//    group and group-killing from the producer's backstop — isn't worth the
//    platform surface yet.
const SELF_LIMITING_SEND: &str = concat!(
    "zellij pipe --name \"$2\" -- \"$3\" >/dev/null 2>&1 & p=$!; ",
    "( sleep \"$1\"; kill \"$p\" ) >/dev/null 2>&1 & w=$!; ",
    "wait \"$p\" 2>/dev/null; kill \"$w\" 2>/dev/null; exit 0",
);

/// Argv for one self-limiting status broadcast: spawn it and the subtree
/// guarantees its own exit within `timeout_secs` (plus scheduling slack),
/// even if the spawner dies first. POSIX `sh` only — the same portability
/// bar as every other host command this workspace spawns.
pub fn self_limiting_pipe_argv(payload: &str, timeout_secs: u64) -> Vec<String> {
    vec![
        "sh".to_string(),
        "-c".to_string(),
        SELF_LIMITING_SEND.to_string(),
        "zj-radar-pipe".to_string(), // $0 — a label for ps output
        timeout_secs.to_string(),    // $1
        STATUS_PIPE_NAME.to_string(), // $2
        payload.to_string(),         // $3
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_carries_payload_and_deadline_as_positionals() {
        let argv = self_limiting_pipe_argv(r#"{"v":1,"msg":"a b; $(rm)"}"#, 5);
        assert_eq!(argv[0], "sh");
        assert_eq!(argv[1], "-c");
        // The payload rides verbatim as a positional parameter — never
        // interpolated into the script text, so no quoting/escaping exists
        // to get wrong.
        assert_eq!(argv[4], "5");
        assert_eq!(argv[5], STATUS_PIPE_NAME);
        assert_eq!(argv[6], r#"{"v":1,"msg":"a b; $(rm)"}"#);
        assert!(!argv[2].contains("rm"), "script must not embed the payload");
    }

    #[test]
    fn script_arms_a_watchdog_and_disarms_it_after_a_normal_send() {
        // Structural guard on the script itself: the deadline must ride
        // INSIDE the spawned subtree (sleep+kill against the pipe's pid),
        // not in the spawning process — that is the whole point of this
        // module (a killed producer must not orphan a blocked client).
        assert!(SELF_LIMITING_SEND.contains("sleep \"$1\""));
        assert!(SELF_LIMITING_SEND.contains("kill \"$p\""));
        assert!(SELF_LIMITING_SEND.contains("wait \"$p\""));
        assert!(SELF_LIMITING_SEND.contains("kill \"$w\""));
    }

    /// The healthy path must not ride the watchdog: a fast send exits the
    /// wrapper immediately, well before the deadline. Guards the disarm
    /// against regressions like `wait "$p"` becoming a bare `wait` (which
    /// would block on the watchdog's sleep too) — that would stall every
    /// producer hook ~5s per tool call with the whole suite still green,
    /// since the hung-path tests only assert reaping, not latency.
    #[test]
    fn healthy_send_exits_immediately_not_at_the_watchdog_deadline() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let shim = dir.path().join("zellij");
        std::fs::write(&shim, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let argv = self_limiting_pipe_argv("{}", DEFAULT_PIPE_TIMEOUT_SECS);
        let mut path = dir.path().as_os_str().to_owned();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let start = std::time::Instant::now();
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env("PATH", path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "wrapper must exit 0 on a healthy send");
        // Milliseconds when healthy; 2s is pure loaded-CI slack, still well
        // clear of the 5s watchdog a coupled exit would wait on.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "wrapper rode the watchdog instead of exiting with the send ({}ms)",
            start.elapsed().as_millis()
        );
    }

    /// The property the argv exists for, exercised for real: spawn it against
    /// a hanging `zellij` shim, SIGKILL the spawner immediately, and the hung
    /// child is still reaped by the in-subtree watchdog.
    #[test]
    fn subtree_reaps_a_hung_send_even_when_the_spawner_dies() {
        use std::io::Read;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        // Hanging `zellij` shim that reports its own pid, then blocks. `exec`
        // so the reported pid IS the sleeper the watchdog must reap.
        let shim = dir.path().join("zellij");
        std::fs::write(&shim, "#!/bin/sh\necho $$ > \"$(dirname \"$0\")/pid\"\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let argv = self_limiting_pipe_argv("{}", 1);
        let mut path = dir.path().as_os_str().to_owned();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let mut spawner = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env("PATH", path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        // Wait for the shim to be up (pid file written), then kill the
        // spawner — the moment a real hook runner would kill the producer.
        let pid_file = dir.path().join("pid");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let pid = loop {
            if let Ok(mut f) = std::fs::File::open(&pid_file) {
                let mut s = String::new();
                let _ = f.read_to_string(&mut s);
                if let Ok(pid) = s.trim().parse::<u32>() {
                    break pid;
                }
            }
            assert!(std::time::Instant::now() < deadline, "shim never started");
            std::thread::sleep(std::time::Duration::from_millis(25));
        };
        let _ = spawner.kill();
        let _ = spawner.wait();

        // The orphaned subtree must still reap the hung client at its 1s
        // deadline. Poll (with slack for loaded CI) rather than sleep-once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !alive {
                break; // reaped — the property holds
            }
            if std::time::Instant::now() >= deadline {
                let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
                panic!("hung pipe client leaked past the watchdog deadline after spawner death");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
