//! `zj-radar run` — turnkey: own a Zellij config dir and launch it.
//!
//! Side effects live in `run()`; every decision (session name, launch args,
//! which advisories to print) is pure and lives in `plan_run`, mirroring the
//! pure-editor / thin-IO split that `setup.rs` uses.

use super::fsutil::atomic_write;
use super::setup::CODEX_HOOK_MARKER;
use std::path::{Path, PathBuf};

const GRANT_HINT: &str =
    "First run: a permission prompt opens in the center — press y to enable agent status.";
const PRODUCER_HINT: &str = "Agent status off — no producer wired. Run `zj-radar setup` to enable.";

// ── Pure helpers ─────────────────────────────────────────────────────────────

/// Session name from the cwd basename (sanitized) or an explicit override.
/// Zellij session names allow `[A-Za-z0-9_-]`; other chars fold to `-`. If
/// nothing alphanumeric survives (empty or all-symbol basename), falls back to
/// `"radar"` rather than emitting a degenerate all-dashes name.
pub(crate) fn session_name(cwd: &Path, name_override: Option<&str>) -> String {
    if let Some(n) = name_override {
        return n.to_string();
    }
    let base = cwd.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let sanitized: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect();
    if sanitized.chars().any(|c| c.is_ascii_alphanumeric()) {
        sanitized
    } else {
        "radar".to_string()
    }
}

/// Args to CREATE a new session with the rail layout:
/// `zellij --config-dir <dir> --session <name> --new-session-with-layout radar`.
///
/// `--new-session-with-layout` (NOT `--layout`) is required here: when combined
/// with `--session`, a plain `--layout` is interpreted by Zellij as "add a tab to
/// the EXISTING session <name>", which fails ("session not found") when it
/// doesn't exist yet. `--new-session-with-layout` always starts a new session —
/// even when invoked from inside Zellij — using the named layout resolved from
/// `--config-dir`'s `layouts/`.
pub(crate) fn create_session_args(config_dir: &Path, session: &str, layout: &str) -> Vec<String> {
    vec![
        "--config-dir".into(),
        config_dir.to_string_lossy().into_owned(),
        "--session".into(),
        session.into(),
        "--new-session-with-layout".into(),
        layout.into(),
    ]
}

/// Args to attach to (or resurrect) an existing session. The layout was applied
/// at creation, so no layout/config-dir flags are needed.
pub(crate) fn attach_session_args(session: &str) -> Vec<String> {
    vec!["attach".into(), session.into()]
}

/// True iff `permissions.kdl` contains a top-level grant block whose quoted key
/// equals `wasm_abs_path`. Zellij keys grants by the literal path string, so an
/// exact match (closing quote included) is correct; the `{` guard skips a bare
/// quoted string that isn't a block header.
pub(crate) fn wasm_is_granted(permissions_kdl: &str, wasm_abs_path: &str) -> bool {
    let needle = format!("\"{wasm_abs_path}\"");
    permissions_kdl
        .lines()
        .map(str::trim_start)
        .any(|l| l.starts_with(&needle) && l.contains('{'))
}

/// Producer-detection advisory: `Some(hint)` when NO producer is wired (Codex
/// hooks lack our marker AND the Claude producer plugin is absent), else `None`.
pub(crate) fn producer_hint(codex_hooks: Option<&str>, claude_present: bool) -> Option<String> {
    let codex = codex_hooks.is_some_and(|h| h.contains(CODEX_HOOK_MARKER));
    if codex || claude_present {
        None
    } else {
        Some(PRODUCER_HINT.to_string())
    }
}

/// True iff Claude Code's installed-plugins manifest lists zj-radar's producer
/// plugin (`zj-radar-claude`). `None`/empty input returns `false`.
pub(crate) fn claude_producer_wired(installed_plugins_json: Option<&str>) -> bool {
    installed_plugins_json.is_some_and(|s| s.contains("zj-radar-claude"))
}

// ── Path locators ────────────────────────────────────────────────────────────

/// The zj-radar–owned Zellij config directory rooted under `data_dir`.
pub(crate) fn owned_config_dir_in(data_dir: &Path) -> PathBuf {
    data_dir.join("zj-radar").join("zellij")
}

/// Zellij's `permissions.kdl` rooted under `cache_dir`. The sub-folder differs
/// between macOS (`org.Zellij-Contributors.Zellij`) and Linux (`zellij`).
pub(crate) fn permissions_path_in(cache_dir: &Path, is_macos: bool) -> PathBuf {
    let folder = if is_macos { "org.Zellij-Contributors.Zellij" } else { "zellij" };
    cache_dir.join(folder).join("permissions.kdl")
}

/// Platform-resolved owned config dir, or `None` if the data dir is unknown.
pub(crate) fn owned_config_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| owned_config_dir_in(&d))
}

/// Platform-resolved path to Zellij's `permissions.kdl`, or `None` if the cache
/// dir is unknown.
pub(crate) fn zellij_permissions_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| permissions_path_in(&c, cfg!(target_os = "macos")))
}

// ── Materializer ─────────────────────────────────────────────────────────────

pub(crate) struct Assets {
    pub config_template: &'static str,
    pub layout: &'static str,
    /// First-run layout: the rail plus a floating pane that hosts the grant
    /// prompt legibly. Selected by `plan_run` only when the wasm isn't granted.
    pub onboarding_layout: &'static str,
    /// `Some` when the wasm is baked into the binary (every prebuilt install);
    /// `None` for a from-crates.io `cargo install`, where `run` downloads it.
    pub wasm: Option<&'static [u8]>,
}

pub(crate) struct Materialized {
    pub config_dir: PathBuf,
    pub wasm_path: PathBuf,
}

/// Write the owned config dir idempotently. A no-op when the version marker
/// matches AND all generated files are present (so a deleted file forces a
/// rewrite). Each file is written atomically; the marker is written last, so an
/// interrupted run is re-materialized rather than served half-written.
pub(crate) fn materialize(
    dir: &Path,
    version: &str,
    assets: &Assets,
) -> std::io::Result<Materialized> {
    let wasm_path = dir.join("plugins").join("zj_radar.wasm");
    let config_path = dir.join("config.kdl");
    let layout_path = dir.join("layouts").join("radar.kdl");
    let onboarding_layout_path = dir.join("layouts").join("radar-onboarding.kdl");
    let marker = dir.join(".zj-radar-version");

    let up_to_date = std::fs::read_to_string(&marker).is_ok_and(|v| v == version)
        && wasm_path.exists()
        && config_path.exists()
        && layout_path.exists()
        && onboarding_layout_path.exists();
    if up_to_date {
        return Ok(Materialized { config_dir: dir.to_path_buf(), wasm_path });
    }

    let config = assets.config_template.replace("@WASM@", &wasm_path.to_string_lossy());
    // Write the embedded wasm if we have it; otherwise leave wasm_path for the
    // caller to populate (download). The `up_to_date` check above already gates
    // on wasm_path.exists(), so a not-yet-downloaded wasm never short-circuits.
    if let Some(bytes) = assets.wasm {
        atomic_write(&wasm_path, bytes)?;
    }
    atomic_write(&config_path, config.as_bytes())?;
    atomic_write(&layout_path, assets.layout.as_bytes())?;
    atomic_write(&onboarding_layout_path, assets.onboarding_layout.as_bytes())?;
    atomic_write(&marker, version.as_bytes())?;
    Ok(Materialized { config_dir: dir.to_path_buf(), wasm_path })
}

// ── Embedded assets ──────────────────────────────────────────────────────────

const CONFIG_TEMPLATE: &str = include_str!("run_assets/config.kdl");
const LAYOUT: &str = include_str!("run_assets/radar.kdl");
const ONBOARDING_LAYOUT: &str = include_str!("run_assets/radar-onboarding.kdl");

// build.rs sets `embedded_wasm` (+ ZJ_RADAR_WASM_PATH) when it has a wasm to
// bake in — true for every prebuilt binary (curl|sh, binstall, nix) and any
// in-workspace build. A from-crates.io `cargo install` has no wasm to embed, so
// WASM is None and `run` downloads the matching release on first use.
#[cfg(embedded_wasm)]
const WASM: Option<&[u8]> = Some(include_bytes!(env!("ZJ_RADAR_WASM_PATH")));
#[cfg(not(embedded_wasm))]
const WASM: Option<&[u8]> = None;

fn embedded_assets() -> Assets {
    Assets {
        config_template: CONFIG_TEMPLATE,
        layout: LAYOUT,
        onboarding_layout: ONBOARDING_LAYOUT,
        wasm: WASM,
    }
}

// ── Orchestration: pure plan + thin IO ───────────────────────────────────────

/// Inputs gathered from the environment, separated from the decision so that
/// `plan_run` is pure and unit-testable.
struct RunFacts {
    /// Resolved session name (cwd basename or override).
    session: String,
    config_dir: PathBuf,
    wasm_path: PathBuf,
    /// Whether a Zellij session of this name already exists (attach vs create).
    session_exists: bool,
    /// Whether we're invoked from inside a Zellij session (`run` can't nest).
    inside_zellij: bool,
    permissions_kdl: Option<String>,
    codex_hooks: Option<String>,
    installed_plugins: Option<String>,
}

/// What to launch and what to advise — the pure result of `plan_run`.
struct RunPlan {
    args: Vec<String>,
    advisories: Vec<String>,
    /// When set, the caller must NOT launch (we're nested) — show guidance.
    nested: bool,
}

/// Pure decision: attach if the session exists, otherwise create with the rail
/// layout; collect the ordered advisory lines (grant hint before producer hint);
/// surface whether we're nested.
fn plan_run(facts: &RunFacts) -> RunPlan {
    let granted = facts
        .permissions_kdl
        .as_deref()
        .is_some_and(|kdl| wasm_is_granted(kdl, &facts.wasm_path.to_string_lossy()));

    let args = if facts.session_exists {
        attach_session_args(&facts.session)
    } else {
        // First run (ungranted) opens the onboarding layout — its floating pane
        // hosts Zellij's grant prompt legibly. Once granted, later runs use the
        // plain rail layout so no transient pane ever flashes.
        let layout = if granted { "radar" } else { "radar-onboarding" };
        create_session_args(&facts.config_dir, &facts.session, layout)
    };

    let mut advisories = Vec::new();
    if !granted {
        advisories.push(GRANT_HINT.to_string());
    }
    let claude = claude_producer_wired(facts.installed_plugins.as_deref());
    if let Some(hint) = producer_hint(facts.codex_hooks.as_deref(), claude) {
        advisories.push(hint);
    }
    RunPlan { args, advisories, nested: facts.inside_zellij }
}

pub struct RunOptions {
    pub name: Option<String>,
    pub print_cmd: bool,
}

/// Read `~/<rel>` if present. Producer/grant probes are strictly read-only.
fn read_under_home(rel: &str) -> Option<String> {
    dirs::home_dir().and_then(|h| std::fs::read_to_string(h.join(rel)).ok())
}

/// True iff a Zellij session named `name` exists (alive or resurrectable), per
/// `zellij list-sessions --short` (plain names, one per line). Any error
/// (zellij missing, no server) is treated as "does not exist" → create path.
fn session_is_live(name: &str) -> bool {
    std::process::Command::new("zellij")
        .args(["list-sessions", "--short"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == name))
}

pub fn run(opts: RunOptions) {
    let Some(dir) = owned_config_dir() else {
        eprintln!("zj-radar: could not resolve a data directory");
        return;
    };
    let materialized = match materialize(&dir, env!("CARGO_PKG_VERSION"), &embedded_assets()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("zj-radar: failed to set up config dir {}: {e}", dir.display());
            return;
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let session = session_name(&cwd, opts.name.as_deref());
    let session_exists = session_is_live(&session);

    // No embedded wasm (a from-crates.io `cargo install`) and none cached yet —
    // fetch the matching release once. Prebuilt binaries embed the wasm, so this
    // path is inert for them.
    if !materialized.wasm_path.exists() {
        let version = super::setup::wasm_download_version();
        if let Err(e) = super::setup::download_wasm_to(&version, &materialized.wasm_path) {
            eprintln!(
                "zj-radar: no embedded wasm and the download failed — {e}\n\
                 Fetch it once while online with: zj-radar setup zellij --download"
            );
            return;
        }
    }

    let facts = RunFacts {
        session,
        config_dir: materialized.config_dir,
        wasm_path: materialized.wasm_path,
        session_exists,
        inside_zellij: std::env::var_os("ZELLIJ").is_some(),
        permissions_kdl: zellij_permissions_path().and_then(|p| std::fs::read_to_string(p).ok()),
        codex_hooks: read_under_home(".codex/hooks.json"),
        installed_plugins: read_under_home(".claude/plugins/installed_plugins.json"),
    };
    let plan = plan_run(&facts);

    for advisory in &plan.advisories {
        println!("{advisory}");
    }
    if opts.print_cmd {
        println!("zellij {}", plan.args.join(" "));
        return;
    }
    if plan.nested {
        eprintln!(
            "zj-radar: you're already inside Zellij. `run` starts a NEW session — detach first \
             (Ctrl-q by default) and re-run, or use `zj-radar setup` to add the rail to your \
             current Zellij config."
        );
        return;
    }
    if let Err(e) = std::process::Command::new("zellij").args(&plan.args).status() {
        eprintln!("zj-radar: failed to launch zellij: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn session_name_sanitizes_falls_back_and_overrides() {
        assert_eq!(session_name(Path::new("/Users/m/dev/zj-radar"), None), "zj-radar");
        assert_eq!(session_name(Path::new("/Users/m/dev/My Proj!"), None), "My-Proj-");
        assert_eq!(session_name(Path::new("/"), None), "radar");
        // All-symbol basename: nothing alphanumeric survives -> fall back, not "---".
        assert_eq!(session_name(Path::new("/Users/m/%%%"), None), "radar");
        assert_eq!(session_name(Path::new("/Users/m/dev/foo"), Some("bar")), "bar");
    }

    #[test]
    fn create_and_attach_args_are_exact() {
        assert_eq!(
            create_session_args(Path::new("/cfg"), "foo", "radar"),
            vec!["--config-dir", "/cfg", "--session", "foo", "--new-session-with-layout", "radar"]
        );
        assert_eq!(attach_session_args("foo"), vec!["attach", "foo"]);
    }

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
    fn grant_detection_matches_block_headers_only() {
        let p = "/Users/m/Library/Application Support/zj-radar/zellij/plugins/zj_radar.wasm";
        assert!(wasm_is_granted(SAMPLE, p));
        assert!(!wasm_is_granted(SAMPLE, "/some/other/zj_radar.wasm"));
        assert!(!wasm_is_granted("", p));
        // A quoted path with no opening brace is not a grant block.
        assert!(!wasm_is_granted("\"/x/zj_radar.wasm\"\n", "/x/zj_radar.wasm"));
        // The closing quote in the needle prevents matching a longer path it prefixes.
        assert!(!wasm_is_granted("\"/x/zj_radar.wasm.bak\" {\n}\n", "/x/zj_radar.wasm"));
    }

    #[test]
    fn locators_compose_expected_paths() {
        assert_eq!(owned_config_dir_in(Path::new("/data")), Path::new("/data/zj-radar/zellij"));
        assert_eq!(
            permissions_path_in(Path::new("/cache"), true),
            Path::new("/cache/org.Zellij-Contributors.Zellij/permissions.kdl")
        );
        assert_eq!(
            permissions_path_in(Path::new("/cache"), false),
            Path::new("/cache/zellij/permissions.kdl")
        );
    }

    fn test_assets() -> Assets {
        Assets {
            config_template: "plugins { radar location=\"file:@WASM@\" {} }\n",
            layout: "layout { default_tab_template { children } tab { pane } }\n",
            onboarding_layout: "layout { tab { pane; floating_panes { pane { plugin location=\"radar\" { role \"onboarding\" } } } } }\n",
            wasm: Some(b"\0asm-dummy"),
        }
    }

    #[test]
    fn materialize_without_embedded_wasm_writes_config_but_not_wasm() {
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        let assets = Assets {
            config_template: "plugins { radar location=\"file:@WASM@\" {} }\n",
            layout: "layout {}\n",
            onboarding_layout: "layout {}\n",
            wasm: None,
        };
        let m = materialize(&dir, "0.1.0", &assets).unwrap();
        // No embedded wasm → materialize must not fabricate the file; `run`
        // downloads it. Config/layout/marker are still written.
        assert!(!m.wasm_path.exists(), "must not create a wasm file when none is embedded");
        assert!(dir.join("config.kdl").exists(), "config still written");
        assert!(dir.join("layouts/radar.kdl").exists(), "layout still written");
        assert!(dir.join("layouts/radar-onboarding.kdl").exists(), "onboarding layout written");
        assert!(dir.join(".zj-radar-version").exists(), "marker still written");
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
        let first_layout = std::fs::read_to_string(dir.join("layouts/radar.kdl")).unwrap();
        let sentinel = Assets {
            config_template: "SENTINEL-CONFIG-SHOULD-NOT-BE-WRITTEN\n",
            layout: "SENTINEL-SHOULD-NOT-BE-WRITTEN\n",
            onboarding_layout: "SENTINEL-ONBOARDING-SHOULD-NOT-BE-WRITTEN\n",
            wasm: Some(b"SENTINEL-WASM"),
        };
        materialize(&dir, "0.1.0", &sentinel).unwrap();
        let after_layout = std::fs::read_to_string(dir.join("layouts/radar.kdl")).unwrap();
        assert_eq!(after_layout, first_layout, "matching version must be a no-op");
        assert!(!after_layout.contains("SENTINEL"), "sentinel must not appear in layout file");
    }

    #[test]
    fn materialize_rewrites_when_a_file_is_missing_despite_matching_marker() {
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        std::fs::remove_file(dir.join("config.kdl")).unwrap();
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        assert!(dir.join("config.kdl").exists(), "deleted file must be restored");
    }

    #[test]
    fn materialize_rewrites_on_version_change() {
        let d = tempdir().unwrap();
        let dir = d.path().join("c");
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        materialize(&dir, "0.2.0", &test_assets()).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".zj-radar-version")).unwrap(), "0.2.0");
    }

    #[test]
    fn claude_producer_detection() {
        let with_plugin = r#"{"plugins":["zj-radar-claude","some-other-plugin"]}"#;
        assert!(claude_producer_wired(Some(with_plugin)));
        let without_plugin = r#"{"plugins":["some-other-plugin","another-one"]}"#;
        assert!(!claude_producer_wired(Some(without_plugin)));
        assert!(!claude_producer_wired(None));
    }

    #[test]
    fn producer_hint_only_when_none_wired() {
        let wired = format!("{CODEX_HOOK_MARKER} zj-radar notify codex");
        assert!(producer_hint(Some(&wired), false).is_none());
        assert!(producer_hint(None, true).is_none());
        assert!(producer_hint(None, false).unwrap().contains("zj-radar setup"));
    }

    #[test]
    fn bundled_layout_has_swaps_and_alias() {
        assert!(LAYOUT.contains("swap_tiled_layout"), "rail layout must declare swaps");
        assert!(LAYOUT.contains("location=\"radar\""), "rail must use the radar alias");
        assert!(CONFIG_TEMPLATE.contains("@WASM@"), "config template needs the @WASM@ token");
    }

    // ── plan_run decision matrix ──
    // `granted`/`codex`/`claude` toggle whether each input signals "already set up".
    // Defaults: session "proj", does not exist (create path), not nested.
    fn facts(granted: bool, codex: bool, claude: bool) -> RunFacts {
        let wasm = "/data/zj-radar/zellij/plugins/zj_radar.wasm";
        RunFacts {
            session: "proj".to_string(),
            config_dir: PathBuf::from("/data/zj-radar/zellij"),
            wasm_path: PathBuf::from(wasm),
            session_exists: false,
            inside_zellij: false,
            permissions_kdl: granted.then(|| format!("\"{wasm}\" {{\n}}\n")),
            codex_hooks: codex.then(|| format!("{CODEX_HOOK_MARKER} zj-radar notify codex")),
            installed_plugins: claude.then(|| "zj-radar-claude".to_string()),
        }
    }

    #[test]
    fn plan_run_creates_new_session_when_absent() {
        let p = plan_run(&facts(true, true, false)); // granted → plain rail layout
        assert_eq!(
            p.args,
            create_session_args(Path::new("/data/zj-radar/zellij"), "proj", "radar")
        );
        assert!(!p.nested);
    }

    #[test]
    fn plan_run_uses_onboarding_layout_when_ungranted() {
        // First run: launch the onboarding layout so the floating pane hosts the
        // grant prompt legibly.
        let p = plan_run(&facts(false, true, false));
        assert_eq!(
            p.args,
            create_session_args(Path::new("/data/zj-radar/zellij"), "proj", "radar-onboarding")
        );
    }

    #[test]
    fn plan_run_attaches_when_session_exists() {
        let mut f = facts(true, true, false);
        f.session_exists = true;
        let p = plan_run(&f);
        assert_eq!(p.args, attach_session_args("proj"));
    }

    #[test]
    fn plan_run_flags_nested_when_inside_zellij() {
        let mut f = facts(true, true, false);
        f.inside_zellij = true;
        assert!(plan_run(&f).nested);
    }

    #[test]
    fn plan_run_advises_grant_when_ungranted() {
        let p = plan_run(&facts(false, true, false)); // producer wired, not granted
        assert_eq!(p.advisories.len(), 1);
        assert!(p.advisories[0].contains("press y"));
    }

    #[test]
    fn plan_run_advises_producer_when_none_wired() {
        let p = plan_run(&facts(true, false, false)); // granted, no producer
        assert_eq!(p.advisories.len(), 1);
        assert!(p.advisories[0].contains("zj-radar setup"));
    }

    #[test]
    fn plan_run_silent_when_granted_and_wired() {
        assert!(plan_run(&facts(true, false, true)).advisories.is_empty()); // granted + claude
        assert!(plan_run(&facts(true, true, false)).advisories.is_empty()); // granted + codex
    }

    #[test]
    fn plan_run_advises_both_when_nothing_set_up() {
        let p = plan_run(&facts(false, false, false));
        assert_eq!(p.advisories.len(), 2);
        assert!(p.advisories[0].contains("press y"), "grant hint comes first");
        assert!(p.advisories[1].contains("zj-radar setup"));
    }
}
