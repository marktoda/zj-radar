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

mod support;

use assert_cmd::Command;
use support::ShimDir;
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

// ── Test 4pre: --inject with NO layout file creates the full layout ──────────
// A stock Zellij ships no layout file at all — this is most first installs.
// The old behavior printed a fragment snippet with nothing to paste it into
// (a dead end); with --inject consent, the full known-good layout is created.

#[test]
fn setup_zellij_inject_creates_full_layout_when_none_exists() {
    let config_dir = TempDir::new().unwrap(); // no layouts/ at all
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    let created = fs::read_to_string(&layout_path).unwrap();
    assert!(
        created.contains("plugin location=\"radar\""),
        "created layout must carry the rail; got:\n{created}"
    );
    assert!(
        created.contains("swap_tiled_layout"),
        "created layout must carry the swap layouts; got:\n{created}"
    );
    assert!(
        !layout_path.with_file_name("default.kdl.zj-radar.bak").exists(),
        "a freshly created layout has no original to back up"
    );
}

#[test]
fn setup_zellij_yes_never_creates_a_layout_file() {
    // --yes takes the safe non-mutating default: snippet + guidance only.
    let config_dir = TempDir::new().unwrap();
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .output()
        .unwrap();

    assert!(!layout_path.exists(), "--yes must not create files");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--inject"),
        "the no-layout fallback must point at the create route; got:\n{stdout}"
    );
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

// ── Test 4c2: --uninstall deletes a layout that setup created whole ───────────
// The no-layout `--inject` path writes `full_layout()` as a new file — no
// markers, but it references the `radar` alias. Uninstall strips the alias, so
// leaving that layout behind would strand the next plain Zellij launch on a
// dead alias. Byte-identical to the generated layout → it is entirely ours →
// delete it.

#[test]
fn setup_zellij_uninstall_deletes_layout_setup_created_whole() {
    let config_dir = TempDir::new().unwrap(); // no layouts/: --inject creates whole
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();
    assert!(layout_path.exists(), "prerequisite: --inject must have created the layout");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--uninstall"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    assert!(
        !layout_path.exists(),
        "--uninstall must delete a layout setup created whole (it references the \
         removed `radar` alias and contains nothing of the user's)"
    );
}

// ── Test 4c3: --uninstall never deletes an edited marker-less layout ──────────
// Same starting point, but the user has edited the file since: no longer
// byte-identical to `full_layout()`, so it must survive — with an advisory
// naming the dead `radar` alias reference instead of a silent strand.

#[test]
fn setup_zellij_uninstall_advises_on_edited_whole_created_layout() {
    let config_dir = TempDir::new().unwrap();
    let layout_path = config_dir.path().join("layouts").join("default.kdl");

    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success();

    // A user edit: the file is no longer provably ours.
    let mut edited = fs::read_to_string(&layout_path).unwrap();
    edited.push_str("// my customization\n");
    fs::write(&layout_path, &edited).unwrap();

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--uninstall"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();

    assert!(
        layout_path.exists(),
        "an edited layout is the user's — uninstall must never delete it"
    );
    assert_eq!(
        fs::read_to_string(&layout_path).unwrap(),
        edited,
        "uninstall must not rewrite a marker-less layout it can't prove it authored"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("still references the removed `radar` alias"),
        "must warn about the stranded alias reference; stdout:\n{stdout}"
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

// ── Test 6: the grant hint is skipped when permissions.kdl already grants ─────
//
// The install path used to build its `ZellijFacts` with `permissions_text:
// None`, so `granted` was always None and the first-launch grant walkthrough
// printed even for a long-granted install. The probe now reads the same
// `zellij_permissions_path()` the doctor uses: the platform cache dir under
// HOME (macOS: `Library/Caches/org.Zellij-Contributors.Zellij`; elsewhere:
// `$XDG_CACHE_HOME/zellij`), keyed by the absolute wasm destination path.

#[test]
fn setup_zellij_skips_grant_hint_when_already_granted() {
    // Runs one full `setup zellij --wasm … --yes` install in an isolated HOME,
    // optionally pre-seeding a permissions.kdl granting the destination wasm.
    let install_stdout = |seed_grant: bool| -> String {
        let (config_dir, wasm_dir) = isolated_zellij_install_env();
        let wasm_path = wasm_dir.path().join("zj_radar.wasm");
        let home = TempDir::new().unwrap();
        if seed_grant {
            let wasm_dest = config_dir.path().join("plugins").join("zj_radar.wasm");
            #[cfg(target_os = "macos")]
            let perms_path = home
                .path()
                .join("Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl");
            #[cfg(not(target_os = "macos"))]
            let perms_path = home.path().join(".cache/zellij/permissions.kdl");
            fs::create_dir_all(perms_path.parent().unwrap()).unwrap();
            // The FULL permission set the plugin requests — grant detection
            // deliberately rejects partial grants (a stale entry makes Zellij
            // re-prompt illegibly in the rail), so a partial seed here would
            // read as ungranted and the hint would print.
            fs::write(
                &perms_path,
                format!(
                    "\"{}\" {{\n    ReadApplicationState\n    ReadCliPipes\n    \
                     ChangeApplicationState\n    RunCommands\n}}\n",
                    wasm_dest.display()
                ),
            )
            .unwrap();
        }
        let output = Command::cargo_bin("zj-radar")
            .unwrap()
            .args(["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--yes"])
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .assert()
            .success()
            .get_output()
            .clone();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    let stdout = install_stdout(true);
    assert!(
        stdout.contains("zellij: installed"),
        "granted install must still install; stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("press y to"),
        "already-granted install must not print the first-launch grant hint; stdout:\n{stdout}"
    );

    // With no prior grant, a consented install now pre-seeds the grant itself
    // (see the pre-seed section below) — the first-launch hint is superseded.
    let stdout = install_stdout(false);
    assert!(
        stdout.contains("pre-authorized"),
        "ungranted install must pre-seed the grant; stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("press y to"),
        "a pre-seeded install needs no first-launch walkthrough; stdout:\n{stdout}"
    );
}

// ── Pre-seeded permission grant ───────────────────────────────────────────────
//
// Zellij reads permissions.kdl fresh on every plugin load, so a grant written
// at install time auto-resolves the sidebar's first-run prompt — the user
// never meets Zellij's native y/n overlay, which is illegible at rail width
// (zellij#4749) and used to present as a silently blank sidebar. The merge is
// conservative: foreign entries survive byte-for-byte, malformed files are
// refused (Zellij silently resets an unparseable file on its next write), and
// dry-run announces without writing.

/// Zellij's permissions.kdl under an isolated HOME, matching
/// `run::permissions_path_in` per OS (tests set both HOME and XDG_CACHE_HOME).
fn permissions_path(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    return home.join("Library/Caches/org.Zellij-Contributors.Zellij/permissions.kdl");
    #[cfg(not(target_os = "macos"))]
    return home.join(".cache/zellij/permissions.kdl");
}

/// One consented full install (`--wasm … --yes`) against isolated config,
/// HOME, and cache dirs. Returns (config_dir, home, stdout, stderr).
fn preseed_install(seed_permissions: Option<&str>) -> (TempDir, TempDir, String, String) {
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    if let Some(seed) = seed_permissions {
        let perms = permissions_path(home.path());
        fs::create_dir_all(perms.parent().unwrap()).unwrap();
        fs::write(&perms, seed).unwrap();
    }
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--yes"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (config_dir, home, stdout, stderr)
}

const FULL_PERMISSION_SET: [&str; 4] =
    ["ReadApplicationState", "ReadCliPipes", "ChangeApplicationState", "RunCommands"];

#[test]
fn setup_zellij_preseeds_grant_on_install() {
    let (config_dir, home, stdout, _) = preseed_install(None);
    let wasm_dest = config_dir.path().join("plugins").join("zj_radar.wasm");

    let perms = fs::read_to_string(permissions_path(home.path()))
        .expect("a consented install must write permissions.kdl");
    assert!(
        perms.contains(&format!("\"{}\"", wasm_dest.display())),
        "grant must be keyed by the absolute wasm destination:\n{perms}"
    );
    for perm in FULL_PERMISSION_SET {
        assert!(perms.contains(perm), "{perm} missing from the grant:\n{perms}");
    }
    assert!(
        stdout.contains("pre-authorized"),
        "the install epilogue must say the grant was written; stdout:\n{stdout}"
    );

    // The doctor reads the same file the same way: grant must now be ok.
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--check"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .assert()
        .get_output()
        .clone();
    let check = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        check.contains("ok grant"),
        "the doctor must see the pre-seeded grant; check output:\n{check}"
    );
}

#[test]
fn setup_zellij_preseed_preserves_foreign_entries() {
    let foreign = "\"/nix/store/abc-room.wasm\" {\n    ReadApplicationState\n    ChangeApplicationState\n}\n";
    let (config_dir, home, _, _) = preseed_install(Some(foreign));
    let wasm_dest = config_dir.path().join("plugins").join("zj_radar.wasm");

    let perms = fs::read_to_string(permissions_path(home.path())).unwrap();
    assert!(
        perms.starts_with(foreign),
        "another plugin's grant must survive byte-for-byte:\n{perms}"
    );
    assert!(
        perms.contains(&format!("\"{}\"", wasm_dest.display())),
        "our grant must be appended alongside:\n{perms}"
    );
    // Zellij owns this file: the standard .bak restore point must exist.
    let bak = permissions_path(home.path()).with_file_name("permissions.kdl.zj-radar.bak");
    assert_eq!(
        fs::read_to_string(&bak).expect(".bak must be written before we touch the file"),
        foreign,
        ".bak must hold the pre-edit contents"
    );
}

#[test]
fn setup_zellij_preseed_refuses_malformed_permissions_kdl() {
    // An unclosed block: Zellij would treat the whole file as empty and reset
    // it on its next write — we must not touch it, and the install must still
    // succeed with the manual first-launch hint as the fallback.
    let malformed = "\"/a.wasm\" {\n    ReadApplicationState\n";
    let (_config_dir, home, stdout, stderr) = preseed_install(Some(malformed));

    assert_eq!(
        fs::read_to_string(permissions_path(home.path())).unwrap(),
        malformed,
        "a malformed permissions.kdl must be left untouched"
    );
    assert!(
        stderr.contains("refusing"),
        "the refusal must be reported; stderr:\n{stderr}"
    );
    assert!(
        stdout.contains("press y to"),
        "with no pre-seed the first-launch hint must return; stdout:\n{stdout}"
    );
}

#[test]
fn setup_zellij_dry_run_would_preseed_but_writes_nothing() {
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--dry-run"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("would pre-authorize"),
        "dry-run must announce the pre-seed; stdout:\n{stdout}"
    );
    assert!(
        !permissions_path(home.path()).exists(),
        "dry-run must not create permissions.kdl"
    );
}

#[test]
fn setup_zellij_preseed_declined_falls_back_to_hint() {
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    // Interactive run: accept the wasm/config prompt ("y"), then EOF declines
    // the layout and pre-authorization prompts.
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--wasm", wasm_path.to_str().unwrap()])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .write_stdin("y\n")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        !permissions_path(home.path()).exists(),
        "a declined pre-authorization must write nothing"
    );
    assert!(
        stdout.contains("press y to"),
        "declining the pre-seed must fall back to the first-launch hint; stdout:\n{stdout}"
    );
}

// ── Test: a symlinked layout is managed — never rewritten, never deleted ──────
// `config_is_managed` (the Nix / home-manager symlink test) used to gate only
// config.kdl; the layout was written via atomic rename, which silently replaces
// a symlink with a regular file — the next `home-manager switch` reverts it and
// the rail "mysteriously vanishes". Both inject and uninstall must refuse.

#[test]
#[cfg(unix)]
fn setup_zellij_never_writes_a_symlinked_layout() {
    let config_dir = isolated_zellij_config(FIXTURE_LAYOUT);
    let layouts = config_dir.path().join("layouts");
    // Move the real layout aside and symlink default.kdl at it (home-manager style).
    let real = config_dir.path().join("hm-source.kdl");
    fs::rename(layouts.join("default.kdl"), &real).unwrap();
    std::os::unix::fs::symlink(&real, layouts.join("default.kdl")).unwrap();

    // Inject: refusal + snippet, symlink and target intact.
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success() // guidance, not a failure — mirrors the managed-config path
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stderr.contains("symlink"),
        "must say WHY it refused (symlink / Nix); stderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Add the sidebar"),
        "must still print the tailored snippet for the Nix config; stdout:\n{stdout}"
    );
    let link = layouts.join("default.kdl");
    assert!(
        fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
        "--inject must not replace the symlink with a regular file"
    );
    assert_eq!(
        fs::read_to_string(&real).unwrap(),
        FIXTURE_LAYOUT,
        "the symlink target must be untouched"
    );

    // Uninstall: same guard (no config.kdl → the layout-only uninstall path).
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--uninstall"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("symlink"),
        "--uninstall must refuse a symlinked layout out loud; stderr:\n{stderr}"
    );
    assert!(
        fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
        "--uninstall must leave the symlink in place"
    );
    assert_eq!(
        fs::read_to_string(&real).unwrap(),
        FIXTURE_LAYOUT,
        "--uninstall must leave the symlink target untouched"
    );
}

// ── Test: an unreadable layout is not an absent one ───────────────────────────
// A layout that exists but fails read_to_string (EACCES, non-UTF8) used to take
// the "No layout at {path} — create it?" path: consenting replaced the user's
// file under a prompt that lied. The error kinds must be discriminated —
// NotFound creates, everything else refuses and leaves the file alone.

#[test]
fn setup_zellij_refuses_an_unreadable_layout_instead_of_recreating_it() {
    let non_utf8: &[u8] = &[0xFF, 0xFE, b'l', b'a', b'y', b'o', b'u', b't'];
    let config_dir = TempDir::new().unwrap();
    let layouts = config_dir.path().join("layouts");
    fs::create_dir_all(&layouts).unwrap();
    let layout_path = layouts.join("default.kdl");
    fs::write(&layout_path, non_utf8).unwrap();

    // Inject: must NOT take the create path (which would replace the file).
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--inject"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .failure() // a real error, unlike the guidance-only symlink refusal
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("could not read layout"),
        "must report the read error, not claim the layout is absent; stderr:\n{stderr}"
    );
    assert_eq!(
        fs::read(&layout_path).unwrap(),
        non_utf8,
        "--inject must leave an unreadable layout byte-identical"
    );

    // Uninstall: same discrimination (unreadable != nothing to uninstall).
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--uninstall"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("could not read layout"),
        "--uninstall must report the read error; stderr:\n{stderr}"
    );
    assert_eq!(
        fs::read(&layout_path).unwrap(),
        non_utf8,
        "--uninstall must leave an unreadable layout byte-identical"
    );
}

// ── Test: --dry-run --download never touches the network ─────────────────────
// --dry-run is documented "Show what would change; write nothing", yet it used
// to fetch the release wasm first — and hard-fail offline. The config splice
// needs only the destination path, so the fetch must be skipped and announced.

#[test]
fn setup_zellij_dry_run_download_skips_the_fetch_and_writes_nothing() {
    let config_dir = TempDir::new().unwrap();
    let emptybin = TempDir::new().unwrap(); // no curl/wget: a fetch attempt would fail

    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "zellij", "--download", "--dry-run"])
        .env("ZELLIJ_CONFIG_DIR", config_dir.path())
        .env("PATH", emptybin.path()) // offline-equivalent: success proves no fetch
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("would download"),
        "dry-run must announce the download it skipped; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("dry-run"),
        "the config preview must still print; stdout:\n{stdout}"
    );
    assert!(
        !config_dir.path().join("config.kdl").exists()
            && !config_dir.path().join("plugins").exists(),
        "--dry-run --download must write nothing"
    );
}

// ── Test: --check conflicts with --uninstall at the CLI boundary ──────────────
// `setup --check --uninstall` used to silently run the doctor and never
// uninstall — reading as "uninstalled". Clap now hard-errors, matching --grant.

#[test]
fn setup_check_conflicts_with_uninstall() {
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "--check", "--uninstall"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("cannot be used with"),
        "clap must reject --check --uninstall as a hard conflict; stderr:\n{stderr}"
    );
}

// ── Test: the doctor flags a ZELLIJ_CONFIG_FILE override ─────────────────────
// Zellij's config precedence puts $ZELLIJ_CONFIG_FILE above --config-dir
// resolution; setup's resolver honors ZELLIJ_CONFIG_DIR/XDG but not this var,
// so setup can edit a config.kdl Zellij never reads while --check reports
// healthy. The doctor must name both paths when they diverge.

#[test]
fn check_warns_when_zellij_config_file_points_elsewhere() {
    let config_dir = TempDir::new().unwrap();
    let resolved = config_dir.path().join("config.kdl");

    let check = |config_file: Option<&std::path::Path>| -> String {
        let mut cmd = Command::cargo_bin("zj-radar").unwrap();
        cmd.args(["setup", "zellij", "--check"])
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env_remove("ZELLIJ_CONFIG_FILE");
        if let Some(f) = config_file {
            cmd.env("ZELLIJ_CONFIG_FILE", f);
        }
        // No `.success()`: the empty fixture has Missing items by design.
        let output = cmd.output().unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    // Override pointing elsewhere: warn, naming both paths.
    let elsewhere = config_dir.path().join("other.kdl");
    let out = check(Some(&elsewhere));
    assert!(
        out.contains("warn config env") && out.contains("$ZELLIJ_CONFIG_FILE"),
        "a diverging ZELLIJ_CONFIG_FILE must produce the config env warning; got:\n{out}"
    );
    assert!(
        out.contains(elsewhere.to_str().unwrap()) && out.contains(resolved.to_str().unwrap()),
        "the warning must name both the override and the resolved path; got:\n{out}"
    );

    // Unset, or pointing at the resolved path: no warning.
    let out = check(None);
    assert!(
        !out.contains("config env"),
        "no override -> no config env item; got:\n{out}"
    );
    let out = check(Some(&resolved));
    assert!(
        !out.contains("config env"),
        "an override that matches the resolved path is consistent; got:\n{out}"
    );
}

// ── setup claude: drives Claude Code's real plugin CLI ───────────────────────
//
// Symmetry with `setup codex`, but through the agent's NATIVE mechanism: the
// `claude plugin` CLI (marketplace add + install), never by editing Claude
// Code's files directly — the marketplace owns the plugin's update channel,
// and a second hand-written wiring would double-fire events.

fn claude_home(wired: bool) -> TempDir {
    let home = TempDir::new().unwrap();
    if wired {
        let plugins = home.path().join(".claude/plugins");
        fs::create_dir_all(&plugins).unwrap();
        fs::write(
            plugins.join("installed_plugins.json"),
            r#"{"plugins":["zj-radar-claude"]}"#,
        )
        .unwrap();
    }
    home
}

fn claude_args(shim: &ShimDir) -> Vec<String> {
    shim.recorded("claude").into_iter().map(|r| r.args.join(" ")).collect()
}

#[test]
fn setup_claude_installs_via_the_plugin_cli() {
    let shim = ShimDir::new();
    shim.add_recorder("claude");
    let home = claude_home(false);
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "claude", "--yes"])
        .env("PATH", shim.path_env())
        .env("HOME", home.path())
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(
        claude_args(&shim),
        vec![
            "plugin marketplace add marktoda/zj-radar".to_string(),
            "plugin install zj-radar-claude@zj-radar".to_string(),
        ],
        "must add the marketplace, then install the plugin; stdout:\n{stdout}"
    );
    assert!(stdout.contains("claude: installed"), "stdout:\n{stdout}");
}

#[test]
fn setup_claude_already_wired_is_a_no_op() {
    let shim = ShimDir::new();
    shim.add_recorder("claude");
    let home = claude_home(true);
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "claude", "--yes"])
        .env("PATH", shim.path_env())
        .env("HOME", home.path())
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(claude_args(&shim).is_empty(), "no plugin CLI calls when already wired");
    assert!(stdout.contains("already wired"), "stdout:\n{stdout}");
}

#[test]
fn setup_claude_dry_run_runs_nothing() {
    let shim = ShimDir::new();
    shim.add_recorder("claude");
    let home = claude_home(false);
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "claude", "--dry-run"])
        .env("PATH", shim.path_env())
        .env("HOME", home.path())
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(claude_args(&shim).is_empty(), "dry-run must not invoke the plugin CLI");
    assert!(
        stdout.contains("plugin marketplace add") && stdout.contains("plugin install"),
        "dry-run must announce both commands; stdout:\n{stdout}"
    );
}

#[test]
fn setup_claude_uninstall_runs_plugin_uninstall() {
    let shim = ShimDir::new();
    shim.add_recorder("claude");
    let home = claude_home(true);
    Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "claude", "--uninstall", "--yes"])
        .env("PATH", shim.path_env())
        .env("HOME", home.path())
        .assert()
        .success();
    assert_eq!(
        claude_args(&shim),
        vec!["plugin uninstall zj-radar-claude".to_string()],
        "uninstall must go through the plugin CLI too"
    );
}

#[test]
fn setup_claude_skips_when_binary_missing() {
    let empty_path = TempDir::new().unwrap();
    let home = claude_home(false);
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "claude", "--yes"])
        .env("PATH", empty_path.path())
        .env("HOME", home.path())
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("claude: skipped"),
        "a claude-less machine skips gracefully, like codex; stdout:\n{stdout}"
    );
}

#[test]
fn setup_claude_declined_runs_nothing() {
    let shim = ShimDir::new();
    shim.add_recorder("claude");
    let home = claude_home(false);
    let output = Command::cargo_bin("zj-radar")
        .unwrap()
        .args(["setup", "claude"])
        .env("PATH", shim.path_env())
        .env("HOME", home.path())
        .write_stdin("")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(claude_args(&shim).is_empty(), "declined consent must not invoke the plugin CLI");
    assert!(stdout.contains("skipped (declined)"), "stdout:\n{stdout}");
}

// ── Review fixes: cross-version uninstall, grant recovery path, dry-run copy ──

#[test]
fn setup_zellij_uninstall_deletes_a_v013_authored_layout() {
    // v0.1.3's create_full_layout named the starter tab "shell"; 0.1.4 removed
    // the name. Uninstall must still recognize the OLD byte-shape as
    // setup-authored, or every 0.1.3-created layout is stranded on a dead
    // alias after the alias strip.
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    let run = |args: &[&str]| {
        Command::cargo_bin("zj-radar")
            .unwrap()
            .args(args)
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .assert()
            .success();
    };
    run(&["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--inject", "--yes"]);
    let layout_path = config_dir.path().join("layouts/default.kdl");
    let current = fs::read_to_string(&layout_path).unwrap();
    let legacy = current.replace("    tab focus=true {", "    tab name=\"shell\" focus=true {");
    assert_ne!(current, legacy, "fixture must actually differ (tab line drifted?)");
    fs::write(&layout_path, &legacy).unwrap();

    run(&["setup", "zellij", "--uninstall", "--yes"]);
    assert!(
        !layout_path.exists(),
        "a layout authored whole by v0.1.3 setup must still be deleted on uninstall"
    );
}

#[test]
fn setup_zellij_yes_preseeds_when_wasm_already_installed() {
    // The doctor's grant remedy says to re-run setup: with the wasm already at
    // the stable path, `setup zellij -y` (no wasm source) must be able to
    // pre-authorize — the grant is keyed by the destination path, which
    // exists. This used to dead-end in RefuseNoWasm/LayoutOnlyInstall.
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    let run = |args: &[&str]| {
        Command::cargo_bin("zj-radar")
            .unwrap()
            .args(args)
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .assert()
            .success();
    };
    run(&["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--inject", "--yes"]);
    fs::remove_file(permissions_path(home.path())).unwrap(); // grant lost (e.g. answered n once)

    run(&["setup", "zellij", "--yes"]); // the doctor's advertised recovery
    assert!(
        permissions_path(home.path()).exists(),
        "wasm-less re-run must restore the grant when the wasm is already installed"
    );
}

#[test]
fn setup_zellij_dry_run_never_contradicts_itself_about_the_grant() {
    // Unchanged arm + --dry-run: "would pre-authorize" and "permissions not
    // pre-authorized — the rail will look BLANK" must not both print.
    let (config_dir, wasm_dir) = isolated_zellij_install_env();
    let wasm_path = wasm_dir.path().join("zj_radar.wasm");
    let home = TempDir::new().unwrap();
    let run = |extra: &[&str]| {
        let mut args = vec!["setup", "zellij", "--wasm", wasm_path.to_str().unwrap(), "--yes"];
        args.extend_from_slice(extra);
        Command::cargo_bin("zj-radar")
            .unwrap()
            .args(&args)
            .env("ZELLIJ_CONFIG_DIR", config_dir.path())
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join(".cache"))
            .assert()
            .success()
            .get_output()
            .clone()
    };
    run(&[]); // real install -> config now up to date
    fs::remove_file(permissions_path(home.path())).unwrap(); // grant gone again
    let output = run(&["--dry-run"]); // Unchanged arm under dry-run
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("would pre-authorize"),
        "dry-run must announce the pre-seed; stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("permissions not pre-authorized"),
        "the BLANK-rail warning contradicts the would-pre-authorize line; stdout:\n{stdout}"
    );
}
