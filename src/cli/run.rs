//! `zj-radar run` — turnkey: own a Zellij config dir and launch it.
use std::path::{Path, PathBuf};

/// Session name derived from the cwd basename (sanitized), or an explicit
/// override. Zellij session names allow [A-Za-z0-9_-]; everything else folds to
/// '-'. Empty/degenerate input falls back to "radar".
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

/// Args to exec: `zellij --config-dir <dir> --layout radar --session <name>`
/// (attach-or-create is Zellij's default for `--session`).
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

pub(crate) fn zellij_permissions_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| permissions_path_in(&c, cfg!(target_os = "macos")))
}

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

const CONFIG_TEMPLATE: &str = include_str!("run_assets/config.kdl");
const LAYOUT: &str = include_str!("run_assets/radar.kdl");
const WASM: &[u8] = include_bytes!(env!("ZJ_RADAR_WASM_PATH"));

fn embedded_assets() -> Assets {
    Assets { config_template: CONFIG_TEMPLATE, layout: LAYOUT, wasm: WASM }
}

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
        // First call: write the real assets.
        materialize(&dir, "0.1.0", &test_assets()).unwrap();
        let first_layout =
            std::fs::read_to_string(dir.join("layouts/radar.kdl")).unwrap();
        // Second call: same version, but with a sentinel layout that must NOT land on disk.
        let sentinel_assets = Assets {
            config_template: "SENTINEL-CONFIG-SHOULD-NOT-BE-WRITTEN\n",
            layout: "SENTINEL-SHOULD-NOT-BE-WRITTEN\n",
            wasm: b"SENTINEL-WASM",
        };
        materialize(&dir, "0.1.0", &sentinel_assets).unwrap();
        let after_layout =
            std::fs::read_to_string(dir.join("layouts/radar.kdl")).unwrap();
        assert_eq!(after_layout, first_layout, "matching version must be a no-op");
        assert!(
            !after_layout.contains("SENTINEL"),
            "sentinel must not appear in layout file"
        );
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
    fn producer_hint_only_when_none_wired() {
        // Codex hooks contain our marker -> producer present -> no hint.
        assert!(producer_hint(Some("ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex"), false, false).is_none());
        // Claude plugin present -> no hint.
        assert!(producer_hint(None, true, false).is_none());
        // Nothing wired -> hint mentions `zj-radar setup`.
        let h = producer_hint(None, false, true).unwrap();
        assert!(h.contains("zj-radar setup"));
    }

    #[test]
    fn bundled_layout_has_swaps_and_alias() {
        assert!(LAYOUT.contains("swap_tiled_layout"), "rail layout must declare swaps");
        assert!(LAYOUT.contains("location=\"radar\""), "rail must use the radar alias");
        assert!(CONFIG_TEMPLATE.contains("@WASM@"), "config template needs the @WASM@ token");
    }
}
