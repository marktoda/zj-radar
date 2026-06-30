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

// ── Test 3: `--wasm` and `--download` are mutually exclusive ─────────────────
// The guard must short-circuit before any download or config write.

#[test]
fn setup_zellij_refuses_wasm_and_download_together() {
    let config_dir = TempDir::new().unwrap();
    let config_path = config_dir.path().join("config.kdl");

    let assert = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--wasm", "/tmp/x.wasm", "--download", "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("not both"),
        "expected a mutual-exclusion refusal; got stderr: {stderr:?}"
    );

    assert!(
        !config_path.exists(),
        "the conflict guard must not write config.kdl"
    );
}

// ── Layout injection tests ────────────────────────────────────────────────────
//
// `setup zellij --inject` (no --wasm / --download) takes the inject-only path:
// it skips the wasm/alias step and operates directly on the target layout.
//
// The fixture layout uses bare booleans (`borderless=true`, `focus=true`) exactly
// as real Zellij layouts are written, which exercises the KDL v1-fallback parser.

const FIXTURE_LAYOUT: &str = "\
layout {
    default_tab_template {
        pane size=1 borderless=true {
            plugin location=\"zellij:tab-bar\"
        }
        children
        pane size=2 borderless=true {
            plugin location=\"zellij:status-bar\"
        }
    }
    tab focus=true {
        pane
    }
}
";

/// Set up a temp config dir with a `layouts/default.kdl` fixture.
fn isolated_zellij_config(layout_text: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let layouts_dir = dir.path().join("layouts");
    fs::create_dir_all(&layouts_dir).unwrap();
    fs::write(layouts_dir.join("default.kdl"), layout_text).unwrap();
    dir
}

// ── Test 4a: --inject writes the rail into the layout and creates a .bak ──────

#[test]
fn setup_zellij_inject_writes_rail_and_bak() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts").join("default.kdl");
    let bak_path    = config_dir.path().join("layouts").join("default.kdl.zj-radar.bak");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    let injected = fs::read_to_string(&layout_path).unwrap();
    assert!(
        injected.contains("// zj-radar:wrap begin"),
        "--inject must add the wrap begin marker; got:\n{injected}"
    );
    assert!(
        injected.contains("plugin location=\"radar\""),
        "--inject must add the radar plugin; got:\n{injected}"
    );
    assert!(
        injected.contains("swap_tiled_layout"),
        "--inject must add swap layouts; got:\n{injected}"
    );
    assert!(
        bak_path.exists(),
        "--inject must create a .zj-radar.bak backup at {}",
        bak_path.display()
    );

    // The backup must be the original fixture.
    let bak = fs::read_to_string(&bak_path).unwrap();
    assert_eq!(
        bak, FIXTURE_LAYOUT,
        ".bak must contain the original layout"
    );
}

// ── Test 4b: --yes without --inject → Snippet: layout unchanged, prints snippet ─

#[test]
fn setup_zellij_yes_without_inject_prints_snippet_and_does_not_modify() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    // --yes without --inject: inject_mode → Snippet (safe default, never mutate).
    // Since there is no --wasm/--download, this takes the layout_only_install path.
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    // Layout must be unmodified.
    let after = fs::read_to_string(&layout_path).unwrap();
    assert_eq!(
        after, FIXTURE_LAYOUT,
        "--yes without --inject must not modify the layout (Snippet mode)"
    );
    // The tailored snippet must be printed.
    assert!(
        stdout.contains("default_tab_template") || stdout.contains("Add the sidebar"),
        "must print the tailored snippet; stdout:\n{stdout}"
    );
}

// ── Test 4c: --uninstall reverses injection ────────────────────────────────────

#[test]
fn setup_zellij_uninstall_reverses_injection() {
    // First inject the rail into the fixture layout.
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    let injected = fs::read_to_string(&layout_path).unwrap();
    assert!(
        injected.contains("// zj-radar:wrap begin"),
        "prerequisite: inject must have written the rail"
    );

    // Now uninstall (layout-only, no wasm config needed).
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--uninstall"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    let after = fs::read_to_string(&layout_path).unwrap();
    assert!(
        !after.contains("// zj-radar:wrap begin") && !after.contains("// zj-radar:block begin"),
        "--uninstall must remove the begin markers"
    );
    assert!(
        !after.contains("plugin location=\"radar\""),
        "--uninstall must remove the radar plugin"
    );
    // Must still be valid KDL with the original tab preserved.
    assert!(
        after.contains("default_tab_template"),
        "uninstall must preserve the rest of the layout"
    );
}

// ── Test 4d: --dry-run prints what would change, writes nothing ────────────────

#[test]
fn setup_zellij_inject_dry_run_prints_and_does_not_write() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layout_path = config_dir.path().join("layouts").join("default.kdl");
    let bak_path    = config_dir.path().join("layouts").join("default.kdl.zj-radar.bak");

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject", "--dry-run"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    // Layout must be unchanged.
    let after = fs::read_to_string(&layout_path).unwrap();
    assert_eq!(after, FIXTURE_LAYOUT, "dry-run must not modify the layout");
    assert!(
        !bak_path.exists(),
        "dry-run must not create a .bak file"
    );

    // stdout must show what would change.
    assert!(
        stdout.contains("dry-run"),
        "dry-run output must mention dry-run; stdout:\n{stdout}"
    );
}
