//! Welds the Claude producer's hooks.json — config the compiler can't see —
//! to the pipe-deadline constants it must stay compatible with. The hook
//! runner kills a hook at its `timeout`; notify.sh's graceful bounded no-op
//! lands at ~cap (the in-subtree watchdog kills the pipe client at the
//! deadline), with the CLI's parent reaper at cap + 1 s as the backstop when
//! that watchdog fails. Equal budgets meant the runner raced — and under load
//! won — against the graceful exit, so every entry must keep
//! `timeout >= cap + 2` (backstop + 1 s spawn/derivation slack).
//!
//! This lives in the plugin crate (not core/cli) because those publish to
//! crates.io and an `include_str!` outside the package would break
//! `cargo package`; this crate is `publish = false`, the same reason
//! `reference_tests.rs` can pin docs/rail-reference.md.

use zj_radar_core::pipe::{DEFAULT_PIPE_TIMEOUT_SECS, MAX_STDIN_BYTES, RUNNING_PIPE_TIMEOUT_SECS};

const HOOKS_JSON: &str = include_str!("../../../plugins/zj-radar-claude/hooks/hooks.json");

/// Every (command, timeout) hook entry in the manifest.
fn entries() -> Vec<(String, u64)> {
    let v: serde_json::Value = serde_json::from_str(HOOKS_JSON).expect("hooks.json parses");
    let mut out = Vec::new();
    for groups in v["hooks"].as_object().expect("hooks map").values() {
        for group in groups.as_array().expect("event group list") {
            for hook in group["hooks"].as_array().expect("hook list") {
                out.push((
                    hook["command"].as_str().expect("command").to_string(),
                    hook["timeout"].as_u64().expect("timeout"),
                ));
            }
        }
    }
    out
}

/// The send deadline an entry's command actually gets: an explicit
/// `ZJ_RADAR_PIPE_TIMEOUT=N` prefix wins (the override both producers honor),
/// else the CLI's status-keyed default — `running` heartbeats take the short
/// cap, everything else the full one (mirrors `default_pipe_timeout_secs` in
/// crates/cli/src/notify.rs and the fallback literals in notify.sh).
fn send_cap(command: &str) -> u64 {
    if let Some(rest) = command.split("ZJ_RADAR_PIPE_TIMEOUT=").nth(1) {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse() {
            return n;
        }
    }
    if command.ends_with(" running") {
        RUNNING_PIPE_TIMEOUT_SECS
    } else {
        DEFAULT_PIPE_TIMEOUT_SECS
    }
}

const NOTIFY_SH: &str = include_str!("../../../plugins/zj-radar-claude/scripts/notify.sh");

/// notify.sh's bash fallback carries hand-typed copies of the pipe-deadline
/// constants (it can't import core::pipe); until now the tie was only a
/// comment. Build the expected lines from the constants themselves so a bump
/// in either place fails here, naming the other.
#[test]
fn bash_producer_deadline_literals_match_the_pipe_constants() {
    let default_line = format!("default_deadline={DEFAULT_PIPE_TIMEOUT_SECS}");
    let running_line =
        format!("[[ \"$status\" == \"running\" ]] && default_deadline={RUNNING_PIPE_TIMEOUT_SECS}");
    assert!(
        NOTIFY_SH.contains(&default_line),
        "notify.sh must set `{default_line}` (core::pipe::DEFAULT_PIPE_TIMEOUT_SECS) — \
         bump the script and the constant together"
    );
    assert!(
        NOTIFY_SH.contains(&running_line),
        "notify.sh must key the running heartbeat via `{running_line}` \
         (core::pipe::RUNNING_PIPE_TIMEOUT_SECS) — bump the script and the constant together"
    );
}

/// notify.sh's stdin cap must equal `core::pipe::MAX_STDIN_BYTES` — the same
/// constant the Rust CLI reads (crates/cli/src/notify.rs), imported rather
/// than hand-copied so a bump can't leave this weld asserting a stale value.
#[test]
fn bash_producer_stdin_cap_matches_the_cli_max_stdin_bytes() {
    let cap_line = format!("head -c {MAX_STDIN_BYTES}");
    assert!(
        NOTIFY_SH.contains(&cap_line),
        "notify.sh must bound stdin with `{cap_line}` \
         (core::pipe::MAX_STDIN_BYTES) — bump the script and the constant together"
    );
}

/// The version triple weld: the workspace version, the crates.io `=X.Y.Z`
/// core pin, and the bundled Claude plugin manifest's version must all move
/// together on a release bump (RELEASING.md). `CARGO_PKG_VERSION` here IS the
/// workspace version (this crate inherits `version.workspace = true`), so a
/// bump that misses any copy fails this test naming the stragglers.
#[test]
fn workspace_core_pin_and_claude_plugin_versions_agree() {
    let version = env!("CARGO_PKG_VERSION");

    let root_manifest = include_str!("../../../Cargo.toml");
    let workspace_line = format!("version = \"{version}\"");
    assert!(
        root_manifest.contains(&workspace_line),
        "root Cargo.toml [workspace.package] must declare `{workspace_line}`"
    );
    let core_pin = format!("version = \"={version}\"");
    assert!(
        root_manifest.contains(&core_pin),
        "root Cargo.toml's zj-radar-core dependency must pin `{core_pin}` \
         (the exact-version crates.io pin — see RELEASING.md)"
    );

    let plugin_json = include_str!("../../../plugins/zj-radar-claude/.claude-plugin/plugin.json");
    let manifest_version = format!("\"version\": \"{version}\"");
    assert!(
        plugin_json.contains(&manifest_version),
        "plugins/zj-radar-claude/.claude-plugin/plugin.json must carry `{manifest_version}`"
    );
}

#[test]
fn every_hook_timeout_clears_its_send_cap_plus_backstop() {
    let es = entries();
    // Non-vacuity: eight events, Notification carries two matchers. A shape
    // change that stops this parser from seeing the entries must fail loud,
    // not pass empty.
    assert_eq!(es.len(), 9, "expected all nine hook entries: {es:?}");
    for (command, timeout) in es {
        let cap = send_cap(&command);
        assert!(
            timeout >= cap + 2,
            "no kill headroom: `{command}` has timeout {timeout} vs send cap {cap} \
             (need >= cap + 2: 1 s reaper backstop + 1 s slack)"
        );
    }
}
