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

// ── Test 2b: codex hook guidance mentions disabled hooks when config says so ──
//
// `print_codex_hook_guidance` writes the `hooks appear disabled` warning to
// STDERR (it's a warning) and the `run \`/hooks\`` line to STDOUT — always,
// disabled or not. Both cases reach it via the same `--yes` install (a fresh
// hooks.json install still lands on the guidance-printing tail).

#[test]
fn setup_codex_guidance_warns_when_hooks_feature_disabled() {
    let codex_home = isolated_codex_home();
    fs::write(
        codex_home.path().join("config.toml"),
        "[features]\nhooks = false\n",
    )
    .unwrap();

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "codex", "--yes"])
        .env("CODEX_HOME", codex_home.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stderr.contains("hooks appear disabled"),
        "config.toml with [features]\\nhooks = false must warn on stderr; got stderr: {stderr:?}"
    );
    assert!(
        stdout.contains("run `/hooks`"),
        "guidance must still print the /hooks reminder on stdout; got stdout: {stdout:?}"
    );
}

#[test]
fn setup_codex_guidance_silent_on_disabled_warning_when_hooks_enabled() {
    // No config.toml at all: `[features].hooks` is unset, so hooks are
    // enabled-or-unset — the disabled warning must not appear, but the
    // `/hooks` reminder still must.
    let codex_home = isolated_codex_home();

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "codex", "--yes"])
        .env("CODEX_HOME", codex_home.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        !stderr.contains("hooks appear disabled"),
        "no config.toml means hooks are not disabled; got stderr: {stderr:?}"
    );
    assert!(
        stdout.contains("run `/hooks`"),
        "guidance must print the /hooks reminder on stdout; got stdout: {stdout:?}"
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

// ── Test: an interrupted download never leaves a partial wasm behind ─────────
// curl/wget write the destination incrementally, so a killed transfer used to
// leave a partial file that the exists()/up-to-date gates then treated as a
// valid wasm forever after (and Zellij would load it with permissions). The
// download must stage to a `.part` sibling and clean it up on failure.

#[test]
fn interrupted_download_leaves_no_partial_wasm() {
    let fakebin = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();
    let tmp = TempDir::new().unwrap();

    // A fake `curl` that mimics an interrupted transfer: writes partial bytes
    // to the `-o` target, then fails.
    let curl = fakebin.path().join("curl");
    fs::write(
        &curl,
        "#!/bin/sh\nprev=\"\"\nfor a in \"$@\"; do\n  [ \"$prev\" = \"-o\" ] && printf 'PARTIAL' > \"$a\"\n  prev=\"$a\"\ndone\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
    }

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--download", "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("PATH", fakebin.path()) // only the fake curl is resolvable
        .env("TMPDIR", tmp.path()) // std::env::temp_dir() lands here
        .assert()
        .failure();

    // Downloads stage in a per-user `zj-radar-<user>/` subdir of the temp root;
    // sweep the whole tree so a leftover wasm/.part/.sha256 anywhere is caught.
    fn sweep(dir: &std::path::Path, hits: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                sweep(&path, hits);
            } else {
                hits.push(path.display().to_string());
            }
        }
    }
    let mut leftovers = Vec::new();
    sweep(tmp.path(), &mut leftovers);
    assert!(
        leftovers.is_empty(),
        "a failed download must leave neither the wasm nor a .part behind, found {leftovers:?}"
    );
}

// ── Test: setup/check operate on the layout Zellij actually loads ────────────
// The layout name resolves --layout → config's `default_layout` → "default".
// Before this, both hardcoded default.kdl: a `default_layout "main"` user got
// the rail injected into a file Zellij never reads, and --check contradicted a
// successful `--inject --layout my` install.

#[test]
fn check_inspects_the_configs_default_layout_and_honors_layout_flag() {
    let config_dir = TempDir::new().unwrap();
    fs::write(
        config_dir.path().join("config.kdl"),
        "default_layout \"main\"\n",
    )
    .unwrap();
    let layouts = config_dir.path().join("layouts");
    fs::create_dir_all(&layouts).unwrap();
    // main.kdl HAS the rail; other.kdl does not.
    fs::write(layouts.join("main.kdl"), "layout {\n    pane\n    // zj-radar:wrap begin\n}\n").unwrap();
    fs::write(layouts.join("other.kdl"), "layout {\n    pane\n}\n").unwrap();

    let check = |extra: &[&str]| {
        let mut args = vec!["setup", "zellij", "--check"];
        args.extend_from_slice(extra);
        // No `.success()`: the doctor exits non-zero when anything is Missing,
        // and this fixture has no wasm — the layout item is the subject here.
        let output = Command::cargo_bin("zj-radar")
            .unwrap()
            .args(&args)
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    // No --layout: the doctor must inspect main.kdl (the default_layout), which
    // has the rail — not default.kdl (absent, would report missing).
    let out = check(&[]);
    assert!(
        out.contains("ok layout"),
        "check must inspect the config's default_layout (main.kdl, has rail); got:\n{out}"
    );

    // --layout other: the doctor must inspect other.kdl, which lacks the rail.
    let out = check(&["--layout", "other"]);
    assert!(
        !out.contains("ok layout"),
        "check --layout other must inspect other.kdl (no rail); got:\n{out}"
    );
}

// ── Test: the doctor is scriptable ───────────────────────────────────────────
// Missing items set the exit code (`setup --check && zj-radar run` can gate),
// and a bare `setup --check` covers BOTH halves instead of silently skipping
// the zellij section the way a bare install (which needs a wasm source) does.

#[test]
fn check_exit_code_gates_and_bare_check_covers_both_targets() {
    let config_dir = TempDir::new().unwrap(); // empty: the zellij half is all Missing
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "--check"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("zellij:"),
        "bare --check must report the zellij half; got:\n{stdout}"
    );
    assert!(
        stdout.contains("codex:"),
        "bare --check must report the codex half; got:\n{stdout}"
    );
    assert!(
        !output.status.success(),
        "missing items must exit non-zero so scripts can gate on the doctor"
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

// ── Test 4a2: --inject with existing swaps skips swap blocks, prints advisory ──

#[test]
fn setup_zellij_inject_with_existing_swaps_skips_swaps_and_advises() {
    // A layout that already declares its own swap_tiled_layout: inject must
    // wrap the rail and add the `ui` template, but never append our swap
    // blocks next to the user's — and it must SAY so, or the first Alt+]
    // silently swaps the rail away.
    let layout_with_swaps = "\
layout {
    default_tab_template {
        children
    }
    swap_tiled_layout name=\"vertical\" {
        tab_template {
            pane split_direction=\"vertical\" {
                pane
                pane
            }
        }
    }
    tab focus=true {
        pane
    }
}
";
    let config_dir = isolated_zellij_config(layout_with_swaps);
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();

    let injected = fs::read_to_string(&layout_path).unwrap();
    assert!(
        injected.contains("plugin location=\"radar\""),
        "--inject must add the radar plugin; got:\n{injected}"
    );
    assert!(
        injected.contains("tab_template name=\"ui\""),
        "--inject must add the ui template; got:\n{injected}"
    );
    assert_eq!(
        injected.matches("swap_tiled_layout").count(), 1,
        "the user's lone swap block must remain the only one; got:\n{injected}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("swap_tiled_layout blocks, which were left untouched"),
        "must print the swap advisory; stdout:\n{stdout}"
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

// ── Test 5: zellij producer hint on the success tail ─────────────────────────
//
// `print_producer_hint_if_needed` runs at the tail of a successful `setup
// zellij` install, on both the `Outcome::Unchanged` arm (config already up to
// date) and the `Outcome::Changed` arm (fresh install). Reaching `Unchanged`
// by hand-authoring a byte-identical `config.kdl` fixture is delicate: the
// managed-alias location string embeds the absolute wasm destination path (or
// a `~/`-relative one when it falls under `HOME`), so any mismatch in that
// string flips `edit_zellij` from `Unchanged` to `Changed`. Rather than guess
// the exact rendering, we prime the `Unchanged` arm for real: run the same
// `setup zellij --wasm <dummy> --yes` invocation twice against the same
// `ZELLIJ_CONFIG_DIR` — the first run performs the real install (`Changed`),
// the second hits `Unchanged` because `edit_zellij` now compares against the
// config that install itself wrote. Both arms print the hint via the same
// `print_producer_hint_if_needed` call, so asserting on the second (`Unchanged`)
// run's output pins that arm specifically.

/// Seed a `ZELLIJ_CONFIG_DIR` tempdir and a dummy (empty, but existing) wasm
/// file so `setup zellij --wasm <path>` passes the `src.is_file()` gate.
fn isolated_zellij_install_env() -> (TempDir, TempDir) {
    let config_dir = TempDir::new().unwrap();
    let wasm_dir = TempDir::new().unwrap();
    fs::write(wasm_dir.path().join("zj_radar.wasm"), b"").unwrap();
    (config_dir, wasm_dir)
}

#[test]
fn setup_zellij_unchanged_arm_hints_producer_when_not_wired() {
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap(); // empty HOME: no producer files at all

    let run = || {
        Command::cargo_bin("zj-radar")
            .unwrap()
            .args(["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--yes"])
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .assert()
            .success()
            .get_output()
            .clone()
    };

    // First run: real install (Changed arm) — establishes the config that the
    // second run will compare against.
    run();
    // Second run: config.kdl now matches what `edit_zellij` would produce ->
    // Unchanged arm, which still calls `print_producer_hint_if_needed`.
    let output = run();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("zellij: config already up to date"),
        "second identical run must hit the Unchanged arm; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("Agent status off — no producer wired"),
        "no producer wired -> the hint must print; stdout:\n{stdout}"
    );
}

#[test]
fn setup_zellij_unchanged_arm_silent_when_producer_wired() {
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    let plugins_dir = home.path().join(".claude/plugins");
    fs::create_dir_all(&plugins_dir).unwrap();
    fs::write(
        plugins_dir.join("installed_plugins.json"),
        r#"{"plugins":["zj-radar-claude"]}"#,
    )
    .unwrap();

    let run = || {
        Command::cargo_bin("zj-radar")
            .unwrap()
            .args(["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--yes"])
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .assert()
            .success()
            .get_output()
            .clone()
    };

    run(); // first: real install (Changed arm)
    let output = run(); // second: Unchanged arm
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("zellij: config already up to date"),
        "second identical run must hit the Unchanged arm; stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("Agent status off — no producer wired"),
        "claude producer wired -> the hint must not print; stdout:\n{stdout}"
    );
}
