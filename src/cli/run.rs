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

pub(crate) fn zellij_permissions_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| permissions_path_in(&c, cfg!(target_os = "macos")))
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
}
