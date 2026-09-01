use super::*;
use super::detect::opencode_plugin_is_ours;

use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) fn opencode_plugin_path() -> Option<PathBuf> {
    opencode_plugins_dir().map(|d| d.join(OPENCODE_PLUGIN_FILE_NAME))
}

/// Read the vendored plugin file for producer *detection* (`run`'s advisory,
/// `setup zellij`'s epilogue hint, `--check`). Routed through
/// [`opencode_plugin_path`] so `$XDG_CONFIG_HOME` is honored on the read side
/// exactly as `setup opencode` honors it on the write side — a hand-rolled
/// `~/.config/opencode` probe here would tell an `XDG_CONFIG_HOME` user their
/// correctly-installed plugin is missing.
pub(crate) fn opencode_plugin_text() -> Option<String> {
    opencode_plugin_path().and_then(|p| std::fs::read_to_string(p).ok())
}

fn opencode_plugins_dir() -> Option<PathBuf> {
    opencode_config_dir().map(|d| d.join("plugins"))
}

fn opencode_config_dir() -> Option<PathBuf> {
    opencode_config_dir_from(std::env::var_os("XDG_CONFIG_HOME"), std::env::var_os("HOME"))
}

/// Resolve opencode's user config home: `$XDG_CONFIG_HOME/opencode` wins, else
/// `$HOME/.config/opencode`. `None` when neither resolves to a usable path.
///
/// Deliberately NOT `dirs::config_dir()` — that yields `~/Library/Application
/// Support` on macOS, but opencode's docs and load order put the user config
/// at `~/.config/opencode` cross-platform (the macOS `Application Support`
/// path is reserved for admin-managed settings, a different precedence tier).
/// Pure (env passed in) so the precedence is unit-tested without touching env.
fn opencode_config_dir_from(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(x) = xdg.filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("opencode"));
    }
    home.filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".config").join("opencode"))
}

fn opencode_installed(opencode_on_path: bool) -> bool {
    opencode_on_path || opencode_plugin_path().is_some_and(|p| p.exists())
}

/// What `setup opencode` found at the plugin path. Three-state on purpose:
/// `Absent` must never collapse into `Text("")` — the foreign-file refusal
/// gate and the `--uninstall` report both key on the difference — and a file
/// that is present but unreadable (non-UTF8, permissions) is not ours to
/// overwrite either.
pub(crate) enum Existing {
    Absent,
    Text(String),
    Unreadable(std::io::Error),
}

fn read_existing(path: &std::path::Path) -> Existing {
    match std::fs::read_to_string(path) {
        Ok(text) => Existing::Text(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Existing::Absent,
        Err(e) => Existing::Unreadable(e),
    }
}

impl Existing {
    /// The plugin text for `OpencodeEnv`, honoring its `None` = absent contract
    /// (an unreadable file has no text to classify, so it is `None` too).
    fn text(&self) -> Option<&str> {
        match self {
            Existing::Text(t) => Some(t),
            Existing::Absent | Existing::Unreadable(_) => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InstallPlan {
    /// Ours and byte-identical to the embedded bridge.
    UpToDate,
    /// A file we can't claim (no marker, or unreadable) and no `--force`.
    RefuseForeign,
    /// Absent, empty, stale-ours, or `--force` over a foreign file.
    Write,
}

/// Pure: the install decision. The embedded JS is the single source of truth,
/// so "already up to date" is a byte-identical compare; a foreign file (no
/// marker) is refused unless `force`, mirroring the codex notify-slot rule.
pub(crate) fn plan_install(existing: &Existing, force: bool) -> InstallPlan {
    match existing {
        Existing::Absent => InstallPlan::Write,
        Existing::Unreadable(_) if force => InstallPlan::Write,
        Existing::Unreadable(_) => InstallPlan::RefuseForeign,
        Existing::Text(text) => {
            let ours = opencode_plugin_is_ours(text);
            if ours && text == OPENCODE_PLUGIN_JS {
                InstallPlan::UpToDate
            } else if !text.is_empty() && !ours && !force {
                InstallPlan::RefuseForeign
            } else {
                InstallPlan::Write
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UninstallPlan {
    /// Nothing at the path — nothing to do, and not an error.
    Absent,
    /// A file is there but carries no marker (or can't be read): leave it.
    NotOurs,
    Remove,
}

/// Pure: the uninstall decision — only a marker-bearing file is ours to delete.
pub(crate) fn plan_uninstall(existing: &Existing) -> UninstallPlan {
    match existing {
        Existing::Absent => UninstallPlan::Absent,
        Existing::Text(text) if opencode_plugin_is_ours(text) => UninstallPlan::Remove,
        Existing::Text(_) | Existing::Unreadable(_) => UninstallPlan::NotOurs,
    }
}

pub(crate) fn setup_opencode(uninstall: bool, opts: OpencodeSetupOpts) {
    if opencode_config_dir().is_none() {
        crate::exit::fail_report(
            "opencode",
            "skipped — set $HOME or $XDG_CONFIG_HOME so the opencode config dir can be resolved",
        );
        return;
    }
    // The plugins dir may not exist yet on a fresh opencode install; create it
    // (and parents) on install so `backup_then_write` can place the file. This
    // is the one mkdir in the setup tree — opencode auto-loads the dir but
    // doesn't guarantee it exists.
    let Some(path) = opencode_plugin_path() else { return };
    let opencode_on_path = which("opencode");
    if !uninstall && !opencode_installed(opencode_on_path) {
        println!("opencode: skipped (binary/config not found)");
        return;
    }
    let existing = read_existing(&path);
    let env = OpencodeEnv {
        opencode_on_path,
        zj_radar_on_path: which("zj-radar"),
        plugin_text: existing.text().map(str::to_string),
    };
    let facts = analyze_opencode(&env);

    if uninstall {
        match plan_uninstall(&existing) {
            UninstallPlan::Absent => {
                println!("opencode: plugin not installed ({})", path.display());
                return;
            }
            UninstallPlan::NotOurs => {
                println!("opencode: plugin not ours (marker absent) — leaving {}", path.display());
                return;
            }
            UninstallPlan::Remove => {}
        }
        if opts.dry_run {
            println!("--- would remove {} (dry-run) ---", path.display());
            return;
        }
        if let Err(e) = std::fs::remove_file(&path) {
            crate::exit::fail_report("opencode", format!("remove failed — {e}"));
        }
        println!("opencode: plugin removed ({})", path.display());
        return;
    }

    match plan_install(&existing, opts.force) {
        InstallPlan::UpToDate => {
            println!("opencode: plugin already up to date ({})", path.display());
            print_opencode_guidance(&facts);
            return;
        }
        InstallPlan::RefuseForeign => {
            let why = match &existing {
                Existing::Unreadable(e) => format!("could not be read ({e})"),
                _ => "is not ours (no marker)".to_string(),
            };
            crate::exit::fail_report(
                "opencode",
                format!(
                    "{} already exists and {why}. Refusing to overwrite it.\n\
                     Re-run with --force to replace it.",
                    path.display()
                ),
            );
            return;
        }
        InstallPlan::Write => {}
    }
    if opts.dry_run {
        println!("--- {} (dry-run) ---\n{OPENCODE_PLUGIN_JS}", path.display());
        print_opencode_guidance(&facts);
        return;
    }
    // Ensure the plugins dir exists before the backup+write (opencode auto-loads
    // it, but a fresh install may not have created it).
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let prompt = format!("Write {}?", path.display());
    if !confirm_and_write("opencode", &path, OPENCODE_PLUGIN_JS, opts.yes, opts.is_tty, &prompt, || Ok(())) {
        return;
    }
    println!("opencode: plugin installed ({})", path.display());
    print_opencode_guidance(&facts);
}

fn print_opencode_guidance(facts: &OpencodeFacts) {
    if !facts.zj_radar_on_path {
        eprintln!(
            "opencode: warning — `zj-radar` not found on PATH; the bridge spawns it per event, \
             so status won't broadcast until it's installed"
        );
    }
    // Plugins load once at opencode startup, so a write mid-session needs a restart.
    println!("opencode: restart opencode (or reload plugins) for the bridge to take effect.");
}

#[cfg(test)]
mod tests {
    use super::super::{OPENCODE_PLUGIN_JS, OPENCODE_PLUGIN_MARKER};
    use super::{opencode_config_dir_from, plan_install, plan_uninstall, Existing, InstallPlan, UninstallPlan};
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn opencode_config_dir_prefers_xdg_over_home() {
        assert_eq!(
            opencode_config_dir_from(Some(os("/x/xdg")), Some(os("/home/u"))),
            Some(PathBuf::from("/x/xdg/opencode")),
        );
    }

    #[test]
    fn opencode_config_dir_falls_back_to_home_dot_config() {
        assert_eq!(
            opencode_config_dir_from(None, Some(os("/home/u"))),
            Some(PathBuf::from("/home/u/.config/opencode")),
        );
    }

    #[test]
    fn opencode_config_dir_is_none_when_neither_resolves() {
        assert_eq!(opencode_config_dir_from(None, None), None);
        // Empty strings are treated as unset, not as the root path.
        assert_eq!(opencode_config_dir_from(Some(OsString::new()), Some(OsString::new())), None);
        assert_eq!(opencode_config_dir_from(None, Some(OsString::new())), None);
        // An empty XDG still lets a real HOME win.
        assert_eq!(
            opencode_config_dir_from(Some(OsString::new()), Some(os("/home/u"))),
            Some(PathBuf::from("/home/u/.config/opencode")),
        );
    }

    fn unreadable() -> Existing {
        Existing::Unreadable(std::io::Error::new(std::io::ErrorKind::InvalidData, "stream did not contain valid UTF-8"))
    }

    #[test]
    fn plan_install_writes_when_absent_or_empty() {
        assert_eq!(plan_install(&Existing::Absent, false), InstallPlan::Write);
        // An empty file is nobody's plugin — write over it without --force.
        assert_eq!(plan_install(&Existing::Text(String::new()), false), InstallPlan::Write);
    }

    #[test]
    fn plan_install_is_up_to_date_only_when_byte_identical() {
        assert_eq!(plan_install(&Existing::Text(OPENCODE_PLUGIN_JS.to_string()), false), InstallPlan::UpToDate);
        // Ours (marker present) but stale → rewrite, no --force needed.
        let stale = format!("// {OPENCODE_PLUGIN_MARKER}\n// older bridge\n");
        assert_eq!(plan_install(&Existing::Text(stale), false), InstallPlan::Write);
    }

    #[test]
    fn plan_install_refuses_foreign_plugin_without_force() {
        let foreign = Existing::Text("export const Other = async () => ({});\n".to_string());
        assert_eq!(plan_install(&foreign, false), InstallPlan::RefuseForeign);
        assert_eq!(plan_install(&foreign, true), InstallPlan::Write);
    }

    #[test]
    fn plan_install_treats_unreadable_file_as_foreign() {
        // A present-but-unreadable file (non-UTF8, permissions) must NOT read as
        // absent — that would skip the refusal gate and overwrite it silently.
        assert_eq!(plan_install(&unreadable(), false), InstallPlan::RefuseForeign);
        assert_eq!(plan_install(&unreadable(), true), InstallPlan::Write);
    }

    #[test]
    fn plan_uninstall_distinguishes_absent_from_foreign() {
        assert_eq!(plan_uninstall(&Existing::Absent), UninstallPlan::Absent);
        assert_eq!(plan_uninstall(&Existing::Text("// not ours\n".to_string())), UninstallPlan::NotOurs);
        assert_eq!(plan_uninstall(&unreadable()), UninstallPlan::NotOurs);
        assert_eq!(plan_uninstall(&Existing::Text(OPENCODE_PLUGIN_JS.to_string())), UninstallPlan::Remove);
    }

    /// Weld: the embedded plugin carries the marker and the spawn contract the
    /// install path, the doctor, and `run`'s detection all key off. A stale
    /// `include_str!` target (renamed file, dropped marker, a `spawnSync` that
    /// would freeze the TUI) is caught here rather than at runtime.
    #[test]
    fn embedded_plugin_carries_marker_and_contract() {
        assert!(
            OPENCODE_PLUGIN_JS.contains(OPENCODE_PLUGIN_MARKER),
            "the vendored plugin must carry the {OPENCODE_PLUGIN_MARKER} marker in its header"
        );
        assert!(
            OPENCODE_PLUGIN_JS.contains("notify opencode"),
            "the bridge must spawn `zj-radar notify opencode`"
        );
        assert!(
            !OPENCODE_PLUGIN_JS.contains("spawnSync"),
            "the bridge must never use spawnSync — it runs in opencode's process and would freeze the TUI"
        );
        assert!(
            OPENCODE_PLUGIN_JS.contains("ZELLIJ"),
            "the bridge must gate on $ZELLIJ (skip spawn when not under Zellij)"
        );
    }
}
