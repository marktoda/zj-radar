//! Integration tests for `zj-radar setup codex` — default hooks.json path.
//!
//! main's `tests/cli.rs` covers: one real run → writes hooks.json with the
//! ZJ_RADAR_CODEX_HOOK=v1 marker (without touching a foreign notify slot).
//!
//! NEW coverage added here:
//!   1. dry-run does NOT write hooks.json; positive control: real run DOES write.
//!   2. idempotency: two real runs → identical hooks.json; first run is non-vacuous.
//!
//! All tests isolate via CODEX_HOME pointing to a tempdir. The `codex_installed()`
//! guard inside setup.rs accepts a pre-existing hooks.json, so we seed the
//! tempdir with an empty `{}` to satisfy it without needing a fake binary on PATH.

#![cfg(feature = "cli")]

mod support;

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

const HOOK_MARKER: &str = "ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex";

/// Returns a fresh tempdir with an empty hooks.json pre-created so that
/// `codex_installed()` returns true (it accepts an existing hooks.json).
fn isolated_codex_home() -> TempDir {
    let dir = TempDir::new().unwrap();
    // seed an empty JSON object — codex_installed() checks hooks_path().exists()
    fs::write(dir.path().join("hooks.json"), "{}\n").unwrap();
    dir
}

// ── Test 1: dry-run does not write; positive control confirms it would have ─

#[test]
fn setup_dry_run_does_not_write_hooks_json() {
    let codex_home = isolated_codex_home();
    let hooks_path = codex_home.path().join("hooks.json");

    // dry-run must leave hooks.json unchanged (still the empty `{}` seed)
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "codex", "--dry-run", "--yes"])
        .env("CODEX_HOME", codex_home.path())
        .assert()
        .success();

    let after_dry_run = fs::read_to_string(&hooks_path).unwrap();
    assert_eq!(
        after_dry_run.trim(),
        "{}",
        "dry-run must not modify hooks.json; got: {after_dry_run:?}"
    );

    // Positive control: the same CODEX_HOME without --dry-run MUST install our hooks.
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "codex", "--yes"])
        .env("CODEX_HOME", codex_home.path())
        .assert()
        .success();

    let after_real = fs::read_to_string(&hooks_path).unwrap();
    assert!(
        after_real.contains(HOOK_MARKER),
        "real run must have written our hook command; got: {after_real:?}"
    );
    // Verify the file has the expected shape: our marker appears for multiple events
    assert!(
        after_real.contains("\"Stop\""),
        "hooks.json must contain the Stop event"
    );
    assert!(
        after_real.contains("\"PermissionRequest\""),
        "hooks.json must contain the PermissionRequest event"
    );
}

// ── Test 2: idempotency ─────────────────────────────────────────────────────

#[test]
fn setup_codex_hooks_is_idempotent() {
    let codex_home = isolated_codex_home();
    let hooks_path = codex_home.path().join("hooks.json");

    let run = || {
        Command::cargo_bin("zj-radar")
            .unwrap()
            .args(["setup", "codex", "--yes"])
            .env("CODEX_HOME", codex_home.path())
            .assert()
            .success();
    };

    // First run installs
    run();
    let after_first = fs::read_to_string(&hooks_path).unwrap();

    // Non-vacuous: first run actually wrote our hook
    assert!(
        after_first.contains(HOOK_MARKER),
        "first run must have written our hook command; got: {after_first:?}"
    );

    // Second run must not change the file
    run();
    let after_second = fs::read_to_string(&hooks_path).unwrap();

    assert_eq!(
        after_first, after_second,
        "second setup must be a no-op (idempotent)"
    );
}
