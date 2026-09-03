//! `zj-radar notify <agent>` — the host shell: read the hook payload, derive an
//! update behind the agent-intake seam, resolve repo/branch, broadcast.
//! All agent-specific decisions live in `agents/`; this file is agent-agnostic
//! plumbing plus the genuinely host-bound helpers (env, stdin). Repo/branch
//! resolution is `git.rs`; the last-sent dedup is `dedup.rs`.

use super::agents::{Agent, AgentUpdate, Intake};
use crate::dedup::{LastSent, SentKey};
use crate::payload::{to_wire, StatusPayload};
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

/// The pane id, with a stderr hint under `--dry-run` when there isn't one.
/// Live hooks stay silent outside Zellij (fire-and-forget must never break
/// the calling hook), but a dry run is explicitly a debugging tool — its
/// least debuggable outcome would be silently printing nothing.
fn pane_id_or_dry_run_hint(dry_run: bool) -> Option<u32> {
    let pane_id = pane_id_from_env();
    if pane_id.is_none() && dry_run {
        eprintln!("zj-radar: not inside Zellij (no ZELLIJ/ZELLIJ_PANE_ID) — nothing would be broadcast");
    }
    pane_id
}

fn read_stdin() -> String {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    // Hooks always pipe their payload; a TTY on stdin means a human invoked
    // this directly (typically `--dry-run`), where a silent read-until-Ctrl-D
    // presents as a hang. Skip the read and say so — one line, stderr, so a
    // `--dry-run | jq` pipeline's stdout stays machine-readable.
    if stdin.is_terminal() {
        eprintln!("zj-radar: no piped stdin; using empty payload");
        return String::new();
    }
    read_capped(stdin.lock(), crate::pipe::MAX_STDIN_BYTES)
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
    let Some(pane_id) = pane_id_or_dry_run_hint(dry_run) else {
        return;
    };
    let Some(agent) = Agent::from_cli(agent) else {
        let expected = Agent::ALL.iter().map(|a| a.source()).collect::<Vec<_>>().join(" | ");
        eprintln!("zj-radar: unknown agent '{agent}' (expected: {expected} — or `notify generic` for scripts)");
        return;
    };

    // An explicit `--status` must be in the wire vocabulary. Without this
    // guard a typo lenient-parses to `idle` inside the adapter and silently
    // ERASES the row it meant to update — the same failure `notify generic`
    // hints on. Hint-and-no-op keeps the calling hook unbroken.
    if let Some(token) = status_arg {
        if Status::try_from_wire(token).is_none() {
            eprintln!("zj-radar: unknown --status '{token}' (expected: {})", wire_vocabulary());
            return;
        }
    }

    // Uniform input sourcing: argv `input` if present (Codex's legacy notify),
    // else stdin (Claude and modern Codex hooks). The adapter parses it.
    let raw = input.map(str::to_owned).unwrap_or_else(read_stdin);
    let Some(update) = agent.derive(&Intake {
        raw: &raw,
        status_arg,
    }) else {
        return;
    };

    broadcast(pane_id, update, agent.source(), dry_run);
}

/// `zj-radar notify generic` — the producer for anything that isn't an
/// instrumented agent: deploy scripts, cron jobs, homegrown loops. No hook
/// payload, no adapter — status/msg/task arrive as explicit flags and the
/// broadcast is otherwise identical to an agent's. `source` picks the rail's
/// kind mark (`test`/`build`/`deploy`/`server`/…); anything unrecognized —
/// including the default `generic` — renders as the neutral `⦿ other` mark.
/// Same fire-and-forget contract as the hook path: a missing/unknown status
/// token prints a hint and exits 0; outside Zellij it exits 0 silently
/// (`--dry-run` adds the not-inside-Zellij hint).
pub fn run_generic(
    status: Option<&str>,
    msg: Option<&str>,
    task: Option<&str>,
    source: Option<&str>,
    dry_run: bool,
) {
    let Some(pane_id) = pane_id_or_dry_run_hint(dry_run) else {
        return;
    };
    let Some(update) = generic_update(status, msg, task) else {
        eprintln!(
            "zj-radar: notify generic needs --status <{}> (plus optional --msg, --task, --source)",
            wire_vocabulary()
        );
        return;
    };
    broadcast(pane_id, update, source.unwrap_or("generic"), dry_run);
}

/// The status tokens producers may pass, straight from the table (`Status::ALL`)
/// so a vocabulary change can't leave a stale hint behind.
fn wire_vocabulary() -> String {
    Status::ALL.iter().map(|s| s.as_wire()).collect::<Vec<_>>().join("|")
}

/// The pure half of [`run_generic`]: explicit flags → update. `None` (no
/// broadcast, hint printed by the caller) when the status token is absent or
/// not in the wire vocabulary — a typo'd status silently becoming `idle` would
/// erase the row it meant to update. Mirrors the adapters' conventions: a
/// running row with no message gets the `working` baseline; idle always
/// broadcasts blank; an empty task means "keep the stored label" (wire rule).
fn generic_update(status: Option<&str>, msg: Option<&str>, task: Option<&str>) -> Option<AgentUpdate> {
    let status = Status::try_from_wire(status?)?;
    let msg = crate::agents::baseline_msg(status, msg.unwrap_or(""));
    Some(AgentUpdate {
        status,
        msg,
        cwd: None,
        task: task.map(str::to_string).filter(|t| !t.trim().is_empty()),
    })
}

/// The shared broadcast tail, the choke point every producer path (`run`,
/// `run_generic`, hence claude/codex/opencode/generic) funnels through: drop
/// a redundant `running` repeat, resolve cwd → repo/branch, build the wire
/// payload, `zellij pipe` it (or print it under `--dry-run`), and on a
/// confirmed delivery record it as this pane's last-sent.
fn broadcast(pane_id: u32, update: AgentUpdate, source: &str, dry_run: bool) {
    let task = update.task.unwrap_or_default();
    // Dedup BEFORE the git probes so a skipped send costs no spawn at all.
    // Repo/branch are therefore not in the key (see `dedup`). A dry run is a
    // debugging tool: it neither consults nor touches the record.
    let key = SentKey::new(update.status, source, &update.msg, &task);
    let now = crate::dedup::unix_now();
    let last_sent = if dry_run { None } else { LastSent::from_env(pane_id) };
    if last_sent.as_ref().is_some_and(|l| l.is_duplicate(&key, now)) {
        return;
    }

    let cwd = update
        .cwd
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("PWD").ok())
        .unwrap_or_else(|| ".".to_string());
    let (repo, branch) = crate::git::repo_branch(&cwd);
    // No client-side length caps here: `to_wire` bounds every free-text field
    // at MAX_WIRE_FIELD_CHARS — the single seam every producer path (adapter
    // msg, `notify generic --task`, repo/branch) already flows through — so a
    // pathologically long input can't push the payload past the plugin's
    // MAX_PAYLOAD_BYTES cap or Linux's per-arg E2BIG limit.
    let payload = to_wire(&StatusPayload {
        pane_id,
        status: update.status,
        repo,
        branch,
        msg: update.msg,
        task,
        source: source.to_string(),
        ack: false,
    });

    if dry_run {
        // Machine-readable output goes to stdout (same rule as `run
        // --print-cmd`): `notify … --dry-run | jq` must capture the payload.
        println!("{payload}");
        return;
    }
    // Only a confirmed delivery is worth recording as this pane's last-sent
    // (the wrapper's status is `zellij pipe`'s — core::pipe).
    if send(&payload, update.status) {
        if let Some(last_sent) = last_sent {
            last_sent.record(&key, now);
        }
    }
}

/// Deliver one wire payload through the self-limiting `zellij pipe` subtree;
/// true iff the client confirmed delivery (exit 0).
///
/// `zellij pipe` blocks until every plugin instance consumes the message, so
/// a rail wedged at the permission prompt would hold it forever, and at hook
/// rate that leaks server FDs until the session crashes. The deadline lives
/// INSIDE the spawned subtree (`self_limiting_pipe_argv`'s watchdog) because
/// hook runners kill their hooks and a dying producer must not orphan a
/// blocked client. The parent only waits — `wait_timeout` (a SIGCHLD
/// self-pipe, no polling) with cap + 1 s as the backstop for the
/// watchdog-fork-failed corner, inside the runner's `timeout >= cap + 2`
/// headroom; `Child::kill` on a handle just reported alive makes the pid ours
/// by construction. Killing the wrapper cannot reach the client in that
/// corner (`core::pipe`'s accepted residual); it bounds the hook, which is
/// what the runner needs. Never panics.
fn send(payload: &str, status: Status) -> bool {
    use wait_timeout::ChildExt;
    let timeout = pipe_send_timeout(status);
    let argv = crate::pipe::self_limiting_pipe_argv(payload, timeout.as_secs());
    let Ok(mut child) = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false; // sh missing/unspawnable — same silent no-op as before
    };
    match child.wait_timeout(timeout + std::time::Duration::from_secs(1)) {
        Ok(Some(status)) => status.success(),
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait(); // reap — a kill without a wait leaves a zombie per hook
            false
        }
        Err(_) => false,
    }
}

/// Send deadline for the status broadcast. `ZJ_RADAR_PIPE_TIMEOUT` (integer
/// seconds — shared with notify.sh's bash fallback, keep the two in sync)
/// overrides both defaults; absent that, the default is keyed on the status
/// being sent: `running` heartbeats ride the per-tool-call hot path and are
/// droppable (the next tool event replaces one), so they get the short
/// `RUNNING_PIPE_TIMEOUT_SECS`, while the once-per-turn edges keep the full
/// `DEFAULT_PIPE_TIMEOUT_SECS` — dropping an edge loses real state. Keying
/// the default here (instead of per-entry env prefixes in hooks.json) gives
/// every producer — claude, codex, opencode, generic — the policy from one seam.
/// Clamped to an hour so `timeout + 1 s` (the send's backstop) cannot
/// overflow `Duration` — this module promises the calling hook never sees
/// a panic.
fn pipe_send_timeout(status: Status) -> std::time::Duration {
    parse_pipe_timeout(
        std::env::var("ZJ_RADAR_PIPE_TIMEOUT").ok(),
        default_pipe_timeout_secs(status),
    )
}

/// The status-keyed half of `pipe_send_timeout`, split out (like
/// `parse_pipe_timeout`) so it is testable without racing on process-global
/// env.
fn default_pipe_timeout_secs(status: Status) -> u64 {
    match status {
        Status::Running => crate::pipe::RUNNING_PIPE_TIMEOUT_SECS,
        _ => crate::pipe::DEFAULT_PIPE_TIMEOUT_SECS,
    }
}

/// The parse half of `pipe_send_timeout`, split out so the fallback and the
/// overflow clamp are unit-testable without racing on process-global env.
fn parse_pipe_timeout(raw: Option<String>, default_secs: u64) -> std::time::Duration {
    raw.and_then(|s| s.parse::<u64>().ok())
        .map(|secs| std::time::Duration::from_secs(secs.min(3600)))
        .unwrap_or(std::time::Duration::from_secs(default_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_pipe_timeout tests ---

    #[test]
    fn pipe_timeout_defaults_and_falls_back_on_garbage() {
        // Unset, non-numeric, negative, and suffixed forms all take the given
        // default — fail closed, matching notify.sh's regex guard. Pinned to
        // the core constant so the CLI's watchdog and parent deadline can
        // never drift from what the plugin and docs/producers.md advertise.
        for raw in [None, Some("abc"), Some("-3"), Some("10s"), Some("")] {
            assert_eq!(
                parse_pipe_timeout(raw.map(String::from), crate::pipe::DEFAULT_PIPE_TIMEOUT_SECS)
                    .as_secs(),
                crate::pipe::DEFAULT_PIPE_TIMEOUT_SECS,
                "raw={raw:?}"
            );
        }
        assert_eq!(
            parse_pipe_timeout(Some("2".into()), crate::pipe::DEFAULT_PIPE_TIMEOUT_SECS).as_secs(),
            2
        );
    }

    #[test]
    fn pipe_timeout_default_is_keyed_on_status() {
        // `running` heartbeats are droppable → short cap; edges keep the full
        // default. The env override (tested above via parse_pipe_timeout)
        // beats both. Pinned to the core constants so notify.sh's mirrored
        // literals and the hooks.json headroom guard share one source.
        assert_eq!(
            default_pipe_timeout_secs(Status::Running),
            crate::pipe::RUNNING_PIPE_TIMEOUT_SECS
        );
        for edge in [Status::Done, Status::Pending, Status::Idle] {
            assert_eq!(
                default_pipe_timeout_secs(edge),
                crate::pipe::DEFAULT_PIPE_TIMEOUT_SECS,
                "status={edge:?}"
            );
        }
    }

    #[test]
    fn pipe_timeout_clamps_so_instant_addition_cannot_panic() {
        // u64::MAX seconds would overflow `Instant + Duration` and panic —
        // this module promises the calling hook never sees a panic.
        let d = parse_pipe_timeout(
            Some(u64::MAX.to_string()),
            crate::pipe::DEFAULT_PIPE_TIMEOUT_SECS,
        );
        assert_eq!(d.as_secs(), 3600);
        let _ = d + std::time::Duration::from_secs(1); // the backstop's add must not overflow
    }

    // --- send backstop ---

    /// `send` against a hanging `zellij` shim returns at the backstop, not
    /// with the child, and reports the send as not delivered.
    #[test]
    fn send_returns_undelivered_at_the_backstop_when_the_wrapper_hangs() {
        let dir = tempfile::TempDir::new().unwrap();
        let shim = dir.path().join("sh");
        // A `sh` that ignores its argv and sleeps: models the wrapper whose
        // in-subtree watchdog failed to fork, so nothing else bounds it.
        std::fs::write(&shim, "#!/bin/sh
exec sleep 30
").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut path = dir.path().as_os_str().to_owned();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        // `sh` resolves through PATH inside `send` (the argv's program is
        // `sh`), so a shim dir first on PATH intercepts it. Cap 1 s → the
        // backstop fires at 2 s.
        let start = std::time::Instant::now();
        let delivered = temp_env(&[("PATH", path.to_str().unwrap()), ("ZJ_RADAR_PIPE_TIMEOUT", "1")], || {
            send("{}", Status::Running)
        });
        assert!(!delivered, "a hung wrapper is not a delivery");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "send rode the child instead of the backstop ({}ms)",
            start.elapsed().as_millis()
        );
    }

    /// Run `f` with the given environment variables set, restoring the
    /// previous values afterwards (tests in this module are the only
    /// writers, and cargo runs them in one process).
    fn temp_env<T>(vars: &[(&str, &str)], f: impl FnOnce() -> T) -> T {
        let saved: Vec<_> = vars.iter().map(|(k, _)| (*k, std::env::var_os(k))).collect();
        for (k, v) in vars {
            std::env::set_var(k, v);
        }
        let out = f();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        out
    }

    // --- generic producer ---

    #[test]
    fn generic_update_requires_a_known_status_token() {
        use crate::status::Status;
        assert!(generic_update(None, None, None).is_none());
        // A typo'd status must NOT lenient-parse to idle — that would silently
        // erase the row the script meant to update. Hint-and-no-op instead.
        assert!(generic_update(Some("runnign"), None, None).is_none());
        let u = generic_update(Some("done"), Some("deploy finished"), None).unwrap();
        assert_eq!(u.status, Status::Done);
        assert_eq!(u.msg, "deploy finished");
    }

    #[test]
    fn generic_update_mirrors_adapter_msg_and_task_conventions() {
        // Running with no msg → the "working" baseline (never a blank active row).
        assert_eq!(generic_update(Some("running"), None, None).unwrap().msg, "working");
        // Idle always broadcasts blank, dropping any msg passed alongside.
        assert_eq!(generic_update(Some("idle"), Some("stale"), None).unwrap().msg, "");
        // Task rides only when non-blank (wire rule: empty = keep stored label).
        let u = generic_update(Some("running"), Some("deploying"), Some("nightly deploy")).unwrap();
        assert_eq!(u.task.as_deref(), Some("nightly deploy"));
        assert_eq!(generic_update(Some("done"), None, Some("  ")).unwrap().task, None);
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
