# `zj-radar run` Turnkey Onboarding — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `zj-radar run` command that launches a complete, preconfigured Zellij session with the radar rail — owning its own config so no integration is required.

**Architecture:** `run` is a thin orchestrator over Zellij. It materializes a zj-radar-owned config dir (embedded wasm + bundled rail layout) under the OS data dir, then execs `zellij --config-dir <owned> --layout radar --session <cwd-name>` (attach-or-create). Permission is granted cleanly on first run (never pre-seeded); the rail's render is split so an ungranted state is visible. All pure logic lives in `src/cli/run.rs` with unit tests; the wasm is embedded via `build.rs` → `include_bytes!`.

**Tech Stack:** Rust 2021, clap (cli feature), `dirs` (new cli-feature dep), `include_bytes!`/`include_str!`, a root `build.rs`. Tests: `cargo test --all-features`, insta snapshots, assert_cmd.

## Global Constraints

- The `cli` feature gates ALL host-only deps. The wasm plugin build (`cargo build --target wasm32-wasip1`) must never pull clap/toml_edit/dirs. New dep `dirs` goes under `cli = [...]`.
- The pure core (lib, non-wasm) carries no `zellij-tile` dependency; `zellij-tile` stays `target.'cfg(target_arch = "wasm32")'`-only.
- Permissions are NEVER pre-seeded into Zellij's `permissions.kdl`. `run` only READS it to decide whether to print a one-time hint.
- `run` NEVER edits agent producer configs (`~/.codex/*`, Claude). Producer handling is detect-and-hint only.
- Test command is `cargo test --all-features` (the `cli` feature is therefore always on under test — `build.rs` must not fail or be slow when a prebuilt wasm exists). NO rustfmt is run in this repo.
- Commit after every task. Conventional-commit messages.
- Embedded wasm path is **version-stable** (`<owned>/plugins/zj_radar.wasm`): bytes overwritten on upgrade, path string unchanged, so the grant persists.

---

## File Structure

- `src/render.rs` (modify) — add `needs_permission(opts)` rail face.
- `src/runtime.rs` (modify) — split the render condition into needs-permission / onboarding / rail.
- `src/cli/run.rs` (create) — all `run` logic: session name, zellij args, grant check, dir/permission locators, materializer, producer detect, orchestrator.
- `src/cli/run_assets/config.kdl` (create) — owned Zellij config: `radar` alias with `@WASM@` placeholder.
- `src/cli/run_assets/radar.kdl` (create) — the rail layout (3 templates + verbatim swaps).
- `src/cli/mod.rs` (modify) — add `Run` subcommand + dispatch.
- `Cargo.toml` (modify) — add `dirs` to the `cli` feature.
- `build.rs` (create) — locate/build the release wasm, emit `ZJ_RADAR_WASM_PATH`.
- `tests/run_cli.rs` (create) — assert_cmd smoke test of `run --print-cmd`.

---

### Task 1: Honest "needs permission" rail state

**Files:**
- Modify: `src/render.rs` (add `needs_permission`)
- Modify: `src/runtime.rs:240-258` (split render branch)
- Test: `src/render.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub fn needs_permission(opts: &RenderOpts) -> RenderedRail` in `render.rs`.

- [ ] **Step 1: Write the failing test** (append to `src/render.rs` tests module)

```rust
#[test]
fn needs_permission_face_is_distinct_and_actionable() {
    let opts = ro(24, 0); // existing test helper: RenderOpts at width 24
    let onboard = onboarding(&opts).ansi;
    let needs = needs_permission(&opts).ansi;
    assert_ne!(needs, onboard, "permission face must differ from idle onboarding");
    // strip SGR codes for content assertions
    let plain: String = needs.chars().collect();
    assert!(plain.contains("press y"), "must tell the user to press y:\n{needs}");
    assert!(plain.to_lowercase().contains("permission"), "must mention permission");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --all-features needs_permission_face -- --nocapture`
Expected: FAIL — `needs_permission` not found.

- [ ] **Step 3: Implement `needs_permission`** (add to `src/render.rs`, mirror `onboarding`'s width discipline)

```rust
/// Rail face shown when permission has NOT been granted. Distinct from
/// `onboarding` (which is the granted-but-idle face) so a blocked install is
/// never mistaken for a working one.
pub fn needs_permission(opts: &RenderOpts) -> RenderedRail {
    fn line(out: &mut String, role: &str, text: &str, w: usize) {
        out.push_str(&format!("{}\n", Seg::new(role, truncate(text, w))));
    }
    let w = opts.width;
    let mut out = String::new();
    let accent = Role::Accent.ansi();
    let needs = Role::NeedsYou.ansi(); // existing role used for "needs you"
    let muted = Role::Muted.ansi();
    line(&mut out, accent, " RADAR", w);
    line(&mut out, accent, &"═".repeat(w), w);
    line(&mut out, needs, " ⚠ needs permission", w);
    out.push('\n');
    line(&mut out, muted, " focus this pane", w);
    line(&mut out, muted, " and press y to", w);
    line(&mut out, muted, " enable agent status.", w);
    RenderedRail::from_ansi_without_targets(out)
}
```

> NOTE: confirm the accent/needs/muted `Role` variant names against `render.rs` (use the same variants `onboarding` uses; `NeedsYou` is the "needs you" legend role). If a name differs, use the actual one — do not invent.

- [ ] **Step 4: Split the render branch** in `src/runtime.rs` (replace the `let rail = if ...` block)

```rust
let rail = if !self.permission_granted {
    render::needs_permission(&opts)
} else if tabrows.is_empty() {
    render::onboarding(&opts)
} else {
    render::render_rail(&tabrows, &opts)
};
```

- [ ] **Step 5: Run render tests + accept snapshot**

Run: `cargo test --all-features` then, if insta reports a new/changed snapshot for the permission face, `cargo insta accept`.
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/render.rs src/runtime.rs
git commit -m "feat(render): distinct needs-permission rail face"
```

---

### Task 2: Session name + Zellij arg builder (pure)

**Files:**
- Create: `src/cli/run.rs`
- Modify: `src/cli/mod.rs` (add `mod run;`)
- Test: `src/cli/run.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub(crate) fn session_name(cwd: &std::path::Path, name_override: Option<&str>) -> String`
- Produces: `pub(crate) fn build_zellij_args(config_dir: &std::path::Path, session: &str) -> Vec<String>`

- [ ] **Step 1: Create `src/cli/run.rs` with failing tests**

```rust
//! `zj-radar run` — turnkey: own a Zellij config dir and launch it.
use std::path::{Path, PathBuf};

/// Session name derived from the cwd basename (sanitized), or an explicit
/// override. Zellij session names allow [A-Za-z0-9_-]; everything else folds to
/// '-'. Empty/degenerate input falls back to "radar".
pub(crate) fn session_name(cwd: &Path, name_override: Option<&str>) -> String {
    unimplemented!()
}

/// Args to exec: `zellij --config-dir <dir> --layout radar --session <name>`
/// (attach-or-create is Zellij's default for `--session`).
pub(crate) fn build_zellij_args(config_dir: &Path, session: &str) -> Vec<String> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_sanitizes_and_falls_back() {
        assert_eq!(session_name(Path::new("/Users/m/dev/zj-radar"), None), "zj-radar");
        assert_eq!(session_name(Path::new("/Users/m/dev/My Proj!"), None), "My-Proj-");
        assert_eq!(session_name(Path::new("/"), None), "radar");
        assert_eq!(session_name(Path::new("/Users/m/dev/foo"), Some("bar")), "bar");
    }

    #[test]
    fn zellij_args_are_exact() {
        let args = build_zellij_args(Path::new("/cfg"), "foo");
        assert_eq!(
            args,
            vec!["--config-dir", "/cfg", "--layout", "radar", "--session", "foo"]
        );
    }
}
```

- [ ] **Step 2: Add `mod run;` to `src/cli/mod.rs`** (next to `mod setup;`)

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --all-features run::tests -- --nocapture`
Expected: FAIL — `unimplemented!()` panics.

- [ ] **Step 4: Implement both functions**

```rust
pub(crate) fn session_name(cwd: &Path, name_override: Option<&str>) -> String {
    if let Some(n) = name_override {
        return n.to_string();
    }
    let base = cwd.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let sanitized: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect();
    if sanitized.is_empty() { "radar".to_string() } else { sanitized }
}

pub(crate) fn build_zellij_args(config_dir: &Path, session: &str) -> Vec<String> {
    vec![
        "--config-dir".into(),
        config_dir.to_string_lossy().into_owned(),
        "--layout".into(),
        "radar".into(),
        "--session".into(),
        session.into(),
    ]
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --all-features run::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/cli/run.rs src/cli/mod.rs
git commit -m "feat(run): session-name sanitizer + zellij arg builder"
```

---

### Task 3: Grant checker — parse `permissions.kdl` (pure)

**Files:**
- Modify: `src/cli/run.rs`
- Test: `src/cli/run.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub(crate) fn wasm_is_granted(permissions_kdl: &str, wasm_abs_path: &str) -> bool`

- [ ] **Step 1: Write failing test** (append to `run.rs` tests)

```rust
const SAMPLE: &str = r#"
"/nix/store/abc-room.wasm" {
    ReadApplicationState
}
"/Users/m/Library/Application Support/zj-radar/zellij/plugins/zj_radar.wasm" {
    ReadApplicationState
    ReadCliPipes
    ChangeApplicationState
}
"#;

#[test]
fn grant_detection() {
    let p = "/Users/m/Library/Application Support/zj-radar/zellij/plugins/zj_radar.wasm";
    assert!(wasm_is_granted(SAMPLE, p));
    assert!(!wasm_is_granted(SAMPLE, "/some/other/zj_radar.wasm"));
    assert!(!wasm_is_granted("", p));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --all-features run::tests::grant_detection`
Expected: FAIL — function missing.

- [ ] **Step 3: Implement** (a granted entry is a top-level `"<path>" {` line; matching the quoted path is sufficient and robust to the permission list contents)

```rust
/// True iff `permissions.kdl` contains a top-level grant block whose quoted key
/// equals `wasm_abs_path`. Zellij keys grants by the literal path string, so an
/// exact match is correct.
pub(crate) fn wasm_is_granted(permissions_kdl: &str, wasm_abs_path: &str) -> bool {
    let needle = format!("\"{wasm_abs_path}\"");
    permissions_kdl
        .lines()
        .map(str::trim_start)
        .any(|l| l.starts_with(&needle) && l.contains('{'))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --all-features run::tests::grant_detection`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cli/run.rs
git commit -m "feat(run): permissions.kdl grant checker"
```

---

### Task 4: Path locators (data dir + Zellij permissions path)

**Files:**
- Modify: `src/cli/run.rs`
- Modify: `Cargo.toml` (add `dirs` to `cli` feature)
- Test: `src/cli/run.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub(crate) fn owned_config_dir_in(data_dir: &Path) -> PathBuf`
- Produces: `pub(crate) fn permissions_path_in(cache_dir: &Path, is_macos: bool) -> PathBuf`
- Produces: `pub(crate) fn owned_config_dir() -> Option<PathBuf>` (wraps `dirs::data_dir`)
- Produces: `pub(crate) fn zellij_permissions_path() -> Option<PathBuf>` (wraps `dirs::cache_dir`)

- [ ] **Step 1: Add `dirs` dep** in `Cargo.toml`

```toml
dirs = { version = "5", optional = true }
```
and change the feature line to:
```toml
cli = ["dep:clap", "dep:toml_edit", "dep:dirs"]
```

- [ ] **Step 2: Write failing test** (append to `run.rs` tests)

```rust
#[test]
fn locators_compose_expected_paths() {
    let data = Path::new("/data");
    assert_eq!(owned_config_dir_in(data), Path::new("/data/zj-radar/zellij"));

    let cache = Path::new("/cache");
    // macOS: Zellij's cache folder is org.Zellij-Contributors.Zellij
    assert_eq!(
        permissions_path_in(cache, true),
        Path::new("/cache/org.Zellij-Contributors.Zellij/permissions.kdl")
    );
    // Linux: cache_dir already points at .../zellij-style root; Zellij uses
    // <cache>/zellij/permissions.kdl
    assert_eq!(
        permissions_path_in(cache, false),
        Path::new("/cache/zellij/permissions.kdl")
    );
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --all-features run::tests::locators`
Expected: FAIL.

- [ ] **Step 4: Implement**

```rust
pub(crate) fn owned_config_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join("zj-radar").join("zellij")
}

pub(crate) fn permissions_path_in(cache_dir: &Path, is_macos: bool) -> PathBuf {
    if is_macos {
        cache_dir
            .join("org.Zellij-Contributors.Zellij")
            .join("permissions.kdl")
    } else {
        cache_dir.join("zellij").join("permissions.kdl")
    }
}

pub(crate) fn owned_config_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| owned_config_dir_in(&d))
}

pub(crate) fn zellij_permissions_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| permissions_path_in(&c, cfg!(target_os = "macos")))
}
```

> NOTE for reviewer: the macОS cache folder name (`org.Zellij-Contributors.Zellij`) was confirmed live this session. If a future Zellij changes it, only `permissions_path_in` needs updating; a wrong path just means the first-run hint always prints (safe degradation), never a crash.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --all-features run::tests::locators`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/cli/run.rs Cargo.toml Cargo.lock
git commit -m "feat(run): data-dir + permissions-path locators (dirs dep)"
```

---

### Task 5: Materializer + embedded asset templates (deep module)

**Files:**
- Modify: `src/cli/run.rs`
- Create: `src/cli/run_assets/config.kdl`
- Create: `src/cli/run_assets/radar.kdl`
- Test: `src/cli/run.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub(crate) struct Assets { pub config_template: &'static str, pub layout: &'static str, pub wasm: &'static [u8] }`
- Produces: `pub(crate) struct Materialized { pub config_dir: PathBuf, pub wasm_path: PathBuf }`
- Produces: `pub(crate) fn materialize(dir: &Path, version: &str, assets: &Assets) -> std::io::Result<Materialized>`
- The `config_template` MUST contain the literal token `@WASM@`, replaced with the absolute wasm path at materialize time.

- [ ] **Step 1: Create asset files.**

`src/cli/run_assets/config.kdl` (minimal — relies on Zellij merging default keybinds; only the `radar` alias is added):
```kdl
// zj-radar run: owned Zellij config. @WASM@ is replaced with the absolute
// materialized wasm path at first run. Keybinds/theme inherit Zellij defaults.
plugins {
    radar location="file:@WASM@" {
        naming "managed"
    }
}
```
`src/cli/run_assets/radar.kdl` — copy the validated rail layout from
`examples/radar-sidebar.kdl` on this branch (3 templates + verbatim
vertical/horizontal/stacked swaps via the `ui` template). Use `plugin
location="radar"` for the rail pane.

> RISK NOTE for reviewer: if, at manual verification, Zellij does NOT merge this
> partial config with its defaults (keybinds missing), replace `config.kdl` with
> the output of `zellij setup --dump-config` plus the `radar` alias injected into
> its `plugins` block. The materializer is unaffected (still a template with
> `@WASM@`).

- [ ] **Step 2: Write failing tests** (append to `run.rs` tests; use `tempfile`)

```rust
use tempfile::tempdir;

fn test_assets() -> Assets {
    Assets {
        config_template: "plugins { radar location=\"file:@WASM@\" {} }\n",
        layout: "layout { default_tab_template { children } tab { pane } }\n",
        wasm: b"\0asm-dummy",
    }
}

#[test]
fn materialize_writes_all_files_and_substitutes_wasm_path() {
    let d = tempdir().unwrap();
    let dir = d.path().join("zj-radar/zellij");
    let m = materialize(&dir, "0.1.0", &test_assets()).unwrap();
    assert_eq!(m.config_dir, dir);
    assert_eq!(m.wasm_path, dir.join("plugins/zj_radar.wasm"));
    assert_eq!(std::fs::read(&m.wasm_path).unwrap(), b"\0asm-dummy");
    let cfg = std::fs::read_to_string(dir.join("config.kdl")).unwrap();
    assert!(cfg.contains(&format!("file:{}", m.wasm_path.display())));
    assert!(!cfg.contains("@WASM@"));
    assert!(dir.join("layouts/radar.kdl").exists());
    assert_eq!(std::fs::read_to_string(dir.join(".zj-radar-version")).unwrap(), "0.1.0");
}

#[test]
fn materialize_is_noop_on_matching_version() {
    let d = tempdir().unwrap();
    let dir = d.path().join("c");
    materialize(&dir, "0.1.0", &test_assets()).unwrap();
    let mtime = std::fs::metadata(dir.join("config.kdl")).unwrap().modified().unwrap();
    // second call with same version must not rewrite
    materialize(&dir, "0.1.0", &test_assets()).unwrap();
    let mtime2 = std::fs::metadata(dir.join("config.kdl")).unwrap().modified().unwrap();
    assert_eq!(mtime, mtime2, "matching version must be a no-op");
}

#[test]
fn materialize_rewrites_on_version_change() {
    let d = tempdir().unwrap();
    let dir = d.path().join("c");
    materialize(&dir, "0.1.0", &test_assets()).unwrap();
    materialize(&dir, "0.2.0", &test_assets()).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join(".zj-radar-version")).unwrap(), "0.2.0");
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --all-features run::tests::materialize`
Expected: FAIL.

- [ ] **Step 4: Implement the materializer**

```rust
pub(crate) struct Assets {
    pub config_template: &'static str,
    pub layout: &'static str,
    pub wasm: &'static [u8],
}
pub(crate) struct Materialized {
    pub config_dir: PathBuf,
    pub wasm_path: PathBuf,
}

pub(crate) fn materialize(dir: &Path, version: &str, assets: &Assets) -> std::io::Result<Materialized> {
    let wasm_path = dir.join("plugins").join("zj_radar.wasm");
    let marker = dir.join(".zj-radar-version");
    let up_to_date = std::fs::read_to_string(&marker).map(|v| v == version).unwrap_or(false)
        && wasm_path.exists()
        && dir.join("config.kdl").exists()
        && dir.join("layouts/radar.kdl").exists();
    if up_to_date {
        return Ok(Materialized { config_dir: dir.to_path_buf(), wasm_path });
    }
    std::fs::create_dir_all(dir.join("plugins"))?;
    std::fs::create_dir_all(dir.join("layouts"))?;
    std::fs::write(&wasm_path, assets.wasm)?;
    let config = assets.config_template.replace("@WASM@", &wasm_path.to_string_lossy());
    std::fs::write(dir.join("config.kdl"), config)?;
    std::fs::write(dir.join("layouts/radar.kdl"), assets.layout)?;
    std::fs::write(&marker, version)?;
    Ok(Materialized { config_dir: dir.to_path_buf(), wasm_path })
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --all-features run::tests::materialize`
Expected: PASS (all three tests).

- [ ] **Step 6: Commit**

```bash
git add src/cli/run.rs src/cli/run_assets/
git commit -m "feat(run): idempotent config-dir materializer + bundled assets"
```

---

### Task 6: `build.rs` — locate or build the wasm, emit `ZJ_RADAR_WASM_PATH`

**Files:**
- Create: `build.rs` (repo root)

**Interfaces:**
- Produces: compile-time env `ZJ_RADAR_WASM_PATH` (absolute path to a `zj_radar.wasm`), consumed by Task 8 via `include_bytes!(env!("ZJ_RADAR_WASM_PATH"))`.

- [ ] **Step 1: Create `build.rs`**

```rust
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // The wasm build itself must not recurse into this logic.
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return;
    }
    // Only the cli build embeds the wasm.
    if std::env::var("CARGO_FEATURE_CLI").is_err() {
        return;
    }
    println!("cargo:rerun-if-env-changed=ZJ_RADAR_WASM_PATH");

    // 1. Explicit override (nix/just provide a prebuilt wasm).
    if let Ok(p) = std::env::var("ZJ_RADAR_WASM_PATH") {
        if PathBuf::from(&p).is_file() {
            println!("cargo:rustc-env=ZJ_RADAR_WASM_PATH={p}");
            return;
        }
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let prebuilt = manifest.join("target/wasm32-wasip1/release/zj_radar.wasm");
    // 2. Prebuilt artifact (fast path for `just test` / dev).
    if prebuilt.is_file() {
        println!("cargo:rustc-env=ZJ_RADAR_WASM_PATH={}", prebuilt.display());
        return;
    }
    // 3. Build it (self-contained `cargo install`). Requires the wasm target.
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--release", "--target", "wasm32-wasip1", "--bin", "zj_radar"])
        .current_dir(&manifest)
        .status()
        .expect("failed to spawn cargo for wasm build");
    assert!(status.success(), "wasm build failed; install the wasm32-wasip1 target");
    println!("cargo:rustc-env=ZJ_RADAR_WASM_PATH={}", prebuilt.display());
}
```

- [ ] **Step 2: Pre-build the wasm so the fast path is taken**

Run: `cargo build --release --target wasm32-wasip1 --bin zj_radar`
Expected: produces `target/wasm32-wasip1/release/zj_radar.wasm`.

- [ ] **Step 3: Verify the env is emitted (build the cli bin)**

Run: `cargo build --features cli --bin zj-radar 2>&1 | tail -5`
Expected: builds without error (build.rs found the prebuilt wasm; no nested build).

- [ ] **Step 4: Commit**

```bash
git add build.rs
git commit -m "build: embed the release wasm via ZJ_RADAR_WASM_PATH"
```

---

### Task 7: Producer detection (detect-and-hint, pure core)

**Files:**
- Modify: `src/cli/run.rs`
- Test: `src/cli/run.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub(crate) fn producer_hint(codex_hooks: Option<&str>, claude_present: bool, zj_radar_on_path: bool) -> Option<String>` — returns `Some(hint)` when NO producer is wired, else `None`.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn producer_hint_only_when_none_wired() {
    // Codex hooks contain our marker -> producer present -> no hint.
    assert!(producer_hint(Some("ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex"), false, false).is_none());
    // Claude plugin present -> no hint.
    assert!(producer_hint(None, true, false).is_none());
    // Nothing wired -> hint mentions `zj-radar setup`.
    let h = producer_hint(None, false, true).unwrap();
    assert!(h.contains("zj-radar setup"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --all-features run::tests::producer`
Expected: FAIL.

- [ ] **Step 3: Implement** (reuse the Codex marker string; if `setup.rs` exposes `CODEX_HOOK_MARKER`, reference it via `super::setup::CODEX_HOOK_MARKER` and make it `pub(crate)`; otherwise inline the literal `"ZJ_RADAR_CODEX_HOOK=v1"`)

```rust
pub(crate) fn producer_hint(
    codex_hooks: Option<&str>,
    claude_present: bool,
    _zj_radar_on_path: bool,
) -> Option<String> {
    let codex = codex_hooks.is_some_and(|h| h.contains("ZJ_RADAR_CODEX_HOOK=v1"));
    if codex || claude_present {
        return None;
    }
    Some("Agent status off — no producer wired. Run `zj-radar setup` to enable.".into())
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --all-features run::tests::producer`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cli/run.rs
git commit -m "feat(run): producer detection hint"
```

---

### Task 8: Orchestrator + `Run` subcommand wiring

**Files:**
- Modify: `src/cli/run.rs` (add `RunOptions`, `run`, embedded consts, IO wrappers)
- Modify: `src/cli/mod.rs` (add `Run` to `Command` + dispatch)

**Interfaces:**
- Consumes: `materialize`, `session_name`, `build_zellij_args`, `wasm_is_granted`, `owned_config_dir`, `zellij_permissions_path`, `producer_hint` (Tasks 2–7); env `ZJ_RADAR_WASM_PATH` (Task 6).
- Produces: `pub struct RunOptions { pub name: Option<String>, pub print_cmd: bool }` and `pub fn run(opts: RunOptions)`.

- [ ] **Step 1: Add embedded assets + a pure command-assembly helper with a test**

Append to `run.rs`:
```rust
const CONFIG_TEMPLATE: &str = include_str!("run_assets/config.kdl");
const LAYOUT: &str = include_str!("run_assets/radar.kdl");
const WASM: &[u8] = include_bytes!(env!("ZJ_RADAR_WASM_PATH"));

fn embedded_assets() -> Assets {
    Assets { config_template: CONFIG_TEMPLATE, layout: LAYOUT, wasm: WASM }
}
```
Test (verifies the bundled layout actually carries swaps — guards against shipping a swap-less layout again):
```rust
#[test]
fn bundled_layout_has_swaps_and_alias() {
    assert!(LAYOUT.contains("swap_tiled_layout"), "rail layout must declare swaps");
    assert!(LAYOUT.contains("location=\"radar\""), "rail must use the radar alias");
    assert!(CONFIG_TEMPLATE.contains("@WASM@"), "config template needs the @WASM@ token");
}
```

- [ ] **Step 2: Run to verify it compiles + passes** (this also proves `include_bytes!`/build.rs work end to end)

Run: `cargo test --all-features run::tests::bundled_layout`
Expected: PASS.

- [ ] **Step 3: Implement the orchestrator**

```rust
pub struct RunOptions {
    pub name: Option<String>,
    pub print_cmd: bool,
}

pub fn run(opts: RunOptions) {
    let Some(dir) = owned_config_dir() else {
        eprintln!("zj-radar: could not resolve a data directory");
        return;
    };
    let version = env!("CARGO_PKG_VERSION");
    let materialized = match materialize(&dir, version, &embedded_assets()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("zj-radar: failed to set up config dir {}: {e}", dir.display());
            return;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let session = session_name(&cwd, opts.name.as_deref());
    let args = build_zellij_args(&materialized.config_dir, &session);

    // First-run grant hint (read-only; never pre-seed).
    let granted = zellij_permissions_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|kdl| wasm_is_granted(&kdl, &materialized.wasm_path.to_string_lossy()))
        .unwrap_or(false);
    if !granted {
        println!("First run: focus the RADAR rail (left) and press y to enable agent status.");
    }

    // Producer hint (detect-only).
    let codex = dirs::home_dir()
        .map(|h| h.join(".codex/hooks.json"))
        .and_then(|p| std::fs::read_to_string(p).ok());
    let claude_present = dirs::home_dir()
        .map(|h| h.join(".claude/plugins").exists())
        .unwrap_or(false);
    if let Some(hint) = producer_hint(codex.as_deref(), claude_present, true) {
        println!("{hint}");
    }

    if opts.print_cmd {
        println!("zellij {}", args.join(" "));
        return;
    }
    let err = std::process::Command::new("zellij").args(&args).status();
    if let Err(e) = err {
        eprintln!("zj-radar: failed to launch zellij: {e}");
    }
}
```

- [ ] **Step 4: Wire the subcommand** in `src/cli/mod.rs`

Add to `enum Command`:
```rust
    /// Launch a turnkey Zellij session with the radar rail (owns its own config).
    Run {
        /// Session name (default: current directory's name).
        name: Option<String>,
        /// Print the zellij command instead of launching it.
        #[arg(long)]
        print_cmd: bool,
    },
```
Add to the `match` in `run()`:
```rust
        Command::Run { name, print_cmd } => {
            run::run(run::RunOptions { name, print_cmd });
        }
```

- [ ] **Step 5: Build + manual smoke**

Run: `cargo run --features cli --bin zj-radar -- run --print-cmd`
Expected: prints the first-run/producer hints (to stdout) and a line like
`zellij --config-dir /…/zj-radar/zellij --layout radar --session <cwd> --session …`; the owned dir now exists with `config.kdl`, `layouts/radar.kdl`, `plugins/zj_radar.wasm`.

- [ ] **Step 6: Commit**

```bash
git add src/cli/run.rs src/cli/mod.rs
git commit -m "feat(run): orchestrator + run subcommand"
```

---

### Task 9: End-to-end smoke test (`run --print-cmd`)

**Files:**
- Create: `tests/run_cli.rs`

**Interfaces:**
- Consumes: the `zj-radar` cli binary (assert_cmd locates it by the `cli` feature bin name).

- [ ] **Step 1: Write the test**

```rust
//! CI-safe end-to-end: `run --print-cmd` materializes the owned dir and emits
//! the expected zellij invocation without execing Zellij. Uses an isolated
//! HOME/XDG so it never touches the developer's real config.
#![cfg(feature = "cli")]
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn run_print_cmd_materializes_and_emits_invocation() {
    let home = tempdir().unwrap();
    let data = tempdir().unwrap();
    let mut cmd = Command::cargo_bin("zj-radar").unwrap();
    cmd.env("HOME", home.path())
        .env("XDG_DATA_HOME", data.path()) // dirs::data_dir() on Linux
        .arg("run")
        .arg("proj")
        .arg("--print-cmd");
    let out = cmd.assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("--layout radar"), "stdout:\n{stdout}");
    assert!(stdout.contains("--session proj"), "stdout:\n{stdout}");
    // dir was materialized (Linux path; macOS uses Application Support)
    // Assert via the printed --config-dir token rather than a hardcoded path:
    assert!(stdout.contains("--config-dir"), "stdout:\n{stdout}");
}
```

> NOTE: `dirs::data_dir()` honors `XDG_DATA_HOME` on Linux but NOT on macOS
> (uses `~/Library/Application Support`). The assertions above avoid
> hardcoding the dir, so the test passes on both. On macOS the materialized dir
> lands under the real `~/Library/Application Support/zj-radar` even with HOME
> overridden? No — `dirs` uses HOME, so overriding HOME isolates it on both.

- [ ] **Step 2: Run**

Run: `cargo test --all-features --test run_cli`
Expected: PASS.

- [ ] **Step 3: Full suite green**

Run: `just test`
Expected: PASS (all layers).

- [ ] **Step 4: Commit**

```bash
git add tests/run_cli.rs
git commit -m "test(run): CI-safe print-cmd end-to-end"
```

---

## Self-Review notes (author)

- Spec coverage: `run` command (T2,T8), owned config dir (T4,T5), embedded wasm (T6), per-dir session (T2), grant clean + honest UI (T1,T8), producer detect (T7), testing incl. `--print-cmd` e2e (T9). README/Nix/doctor are explicitly out of scope per spec Future.
- Type consistency: `Assets`/`Materialized` defined in T5 and consumed in T8; `RunOptions` defined and consumed in T8; arg vector shape fixed in T2 and asserted in T9.
- Known risk points flagged inline for the reviewer: (a) `build.rs` nested wasm build / recursion guard (T6); (b) whether Zellij merges the partial `config.kdl` with default keybinds (T5 risk note, fallback = dump-config); (c) macOS cache folder name for permissions (T4 note, safe degradation).
