//! `zj-radar setup opencode` — vendor the two bridge plugins into opencode's
//! auto-loaded global plugins dir. opencode 1.x loads the *file*
//! `plugins/zj-radar.js` (a server plugin); opencode 2.x's TUI discovers the
//! *directory* `plugins/zj-radar/` and runs `tui.js` inside it. Each line
//! ignores the other's shape, so both install side by side and the user's
//! opencode version picks its bridge. Install, uninstall, `--check` and
//! `run`'s detection all key on the header marker (`detect.rs`).

use super::*;
use super::detect::opencode_plugin_is_ours;
use super::vendored::{plan_install, plan_uninstall, read_existing, Existing, InstallPlan, UninstallPlan};

use std::ffi::OsString;
use std::path::PathBuf;

/// The 1.x bridge file: `<config>/plugins/zj-radar.js`.
pub(crate) fn opencode_plugin_path() -> Option<PathBuf> {
    opencode_plugins_dir().map(|d| d.join(OPENCODE_PLUGIN_FILE_NAME))
}

/// The 2.x TUI bridge file: `<config>/plugins/zj-radar/tui.js`.
pub(crate) fn opencode_tui_plugin_path() -> Option<PathBuf> {
    opencode_tui_plugin_dir().map(|d| d.join(OPENCODE_TUI_PLUGIN_FILE_NAME))
}

fn opencode_tui_plugin_dir() -> Option<PathBuf> {
    opencode_plugins_dir().map(|d| d.join(OPENCODE_TUI_PLUGIN_DIR_NAME))
}

/// Read a vendored plugin file for producer *detection* (`run`'s advisory,
/// `setup zellij`'s epilogue hint, `--check`). Routed through the path
/// resolvers so `$XDG_CONFIG_HOME` is honored on the read side exactly as
/// `setup opencode` honors it on the write side — a hand-rolled
/// `~/.config/opencode` probe here would tell an `XDG_CONFIG_HOME` user their
/// correctly-installed plugin is missing.
pub(crate) fn opencode_plugin_text() -> Option<String> {
    opencode_plugin_path().and_then(|p| std::fs::read_to_string(p).ok())
}

pub(crate) fn opencode_tui_plugin_text() -> Option<String> {
    opencode_tui_plugin_path().and_then(|p| std::fs::read_to_string(p).ok())
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
    opencode_installed_from(
        opencode_on_path,
        opencode_config_dir().is_some_and(|d| d.is_dir()),
        opencode_plugin_path().is_some_and(|p| p.exists()) || opencode_tui_plugin_path().is_some_and(|p| p.exists()),
    )
}

/// Pure: is opencode present on this machine? The binary on PATH, or a
/// populated `~/.config/opencode/` (opencode.json, auth — a Nix/bun-run user
/// may have no `opencode` on PATH), or one of our plugins already dropped there.
fn opencode_installed_from(on_path: bool, config_dir_exists: bool, plugin_exists: bool) -> bool {
    on_path || config_dir_exists || plugin_exists
}

/// One vendored bridge: where it lives, what it must contain, what it is for.
struct Bridge {
    /// "opencode 1.x" / "opencode 2.x" — for the user-facing lines.
    line:     &'static str,
    path:     PathBuf,
    embedded: &'static str,
    existing: Existing,
}

fn bridges() -> Option<Vec<Bridge>> {
    let plugin = opencode_plugin_path()?;
    let tui = opencode_tui_plugin_path()?;
    Some(vec![
        Bridge { line: "opencode 1.x", existing: read_existing(&plugin), path: plugin, embedded: OPENCODE_PLUGIN_JS },
        Bridge { line: "opencode 2.x", existing: read_existing(&tui), path: tui, embedded: OPENCODE_TUI_PLUGIN_JS },
    ])
}

fn paths_list(paths: impl Iterator<Item = PathBuf>) -> String {
    paths.map(|p| p.display().to_string()).collect::<Vec<_>>().join(" and ")
}

pub(crate) fn setup_opencode(uninstall: bool, opts: OpencodeSetupOpts) {
    if opencode_config_dir().is_none() {
        crate::exit::fail_report(
            "opencode",
            "skipped — set $HOME or $XDG_CONFIG_HOME so the opencode config dir can be resolved",
        );
        return;
    }
    // The plugins dir (and the 2.x plugin dir inside it) may not exist yet on
    // a fresh opencode install; the atomic write below creates them — after
    // consent, so a declined install leaves nothing behind.
    let Some(bridges) = bridges() else { return };
    let opencode_on_path = which("opencode");
    if !uninstall && !opencode_installed(opencode_on_path) {
        println!("opencode: skipped (binary/config not found)");
        return;
    }
    let env = OpencodeEnv {
        opencode_on_path,
        zj_radar_on_path: which("zj-radar"),
        plugin_text: bridges[0].existing.text().map(str::to_string),
        tui_plugin_text: bridges[1].existing.text().map(str::to_string),
    };
    let facts = analyze_opencode(&env);

    if uninstall {
        uninstall_opencode(&bridges, &opts);
        return;
    }

    let plans: Vec<InstallPlan> =
        bridges.iter().map(|b| plan_install(&b.existing, b.embedded, opts.force, opencode_plugin_is_ours)).collect();
    // Refuse as a unit: a foreign file at either path is a stop, even when the
    // other would write cleanly — a half-installed pair is harder to reason
    // about than "nothing changed, here is why".
    let foreign: Vec<&Bridge> = bridges
        .iter()
        .zip(&plans)
        .filter(|(_, p)| **p == InstallPlan::RefuseForeign)
        .map(|(b, _)| b)
        .collect();
    if !foreign.is_empty() {
        let why = foreign
            .iter()
            .map(|b| match &b.existing {
                Existing::Unreadable(e) => format!("{} could not be read ({e})", b.path.display()),
                _ => format!("{} is not ours (no marker)", b.path.display()),
            })
            .collect::<Vec<_>>()
            .join("; ");
        crate::exit::fail_report(
            "opencode",
            format!("{why}. Refusing to overwrite it.\nRe-run with --force to replace it."),
        );
        return;
    }
    let to_write: Vec<&Bridge> = bridges
        .iter()
        .zip(&plans)
        .filter(|(_, p)| **p == InstallPlan::Write)
        .map(|(b, _)| b)
        .collect();
    if to_write.is_empty() {
        println!(
            "opencode: plugins already up to date ({})",
            paths_list(bridges.iter().map(|b| b.path.clone()))
        );
        print_opencode_guidance(&facts, false);
        return;
    }
    if opts.dry_run {
        for b in &to_write {
            println!("--- {} (dry-run) ---\n{}", b.path.display(), b.embedded);
        }
        print_opencode_guidance(&facts, true);
        return;
    }
    // One consent step for the pair (the same `confirm` every other setup
    // write goes through): a non-tty run without -y skips rather than writing
    // unasked.
    let prompt = format!("Write {}?", paths_list(to_write.iter().map(|b| b.path.clone())));
    if !confirm(&prompt, opts.yes, opts.is_tty) {
        println!("opencode: skipped (declined)");
        return;
    }
    for b in &to_write {
        if let Err(e) = backup_then_write(&b.path, b.embedded) {
            crate::exit::fail_report("opencode", format!("write failed — {e}"));
            return;
        }
        println!("opencode: plugin installed ({}, {})", b.path.display(), b.line);
    }
    print_opencode_guidance(&facts, true);
}

fn uninstall_opencode(bridges: &[Bridge], opts: &OpencodeSetupOpts) {
    let mut to_remove: Vec<&Bridge> = Vec::new();
    for b in bridges {
        match plan_uninstall(&b.existing, opencode_plugin_is_ours) {
            UninstallPlan::Absent => {}
            UninstallPlan::NotOurs => {
                println!("opencode: plugin not ours (marker absent) — leaving {}", b.path.display());
            }
            UninstallPlan::Remove => to_remove.push(b),
        }
    }
    if to_remove.is_empty() {
        if bridges.iter().all(|b| matches!(plan_uninstall(&b.existing, opencode_plugin_is_ours), UninstallPlan::Absent)) {
            println!("opencode: plugin not installed ({})", paths_list(bridges.iter().map(|b| b.path.clone())));
        }
        return;
    }
    let listed = paths_list(to_remove.iter().map(|b| b.path.clone()));
    if opts.dry_run {
        println!("--- would remove {listed} (dry-run) ---");
        return;
    }
    // Same consent step as every other setup write/remove: a non-tty run
    // without -y skips rather than deleting unasked.
    if !confirm(&format!("Remove {listed}?"), opts.yes, opts.is_tty) {
        println!("opencode: skipped (declined)");
        return;
    }
    for b in &to_remove {
        if let Err(e) = std::fs::remove_file(&b.path) {
            crate::exit::fail_report("opencode", format!("remove failed — {e}"));
            continue;
        }
        // The rewrite path's restore point is ours too: a clean uninstall
        // leaves nothing of zj-radar in opencode's plugins dir.
        let _ = std::fs::remove_file(path_with_suffix(&b.path, BACKUP_SUFFIX));
        println!("opencode: plugin removed ({})", b.path.display());
        // The 2.x plugin directory is ours only while it held our file and
        // now holds nothing; `remove_dir` refuses a non-empty dir, which is
        // exactly the "someone else put something here" case where we must
        // leave it. An empty `zj-radar/` dir we never wrote into stays too.
        if let Some(dir) = b.path.parent().filter(|d| d.file_name().is_some_and(|n| n == OPENCODE_TUI_PLUGIN_DIR_NAME)) {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

fn print_opencode_guidance(facts: &OpencodeFacts, wrote: bool) {
    if !facts.zj_radar_on_path {
        eprintln!(
            "opencode: warning — `zj-radar` not found on PATH; the bridge spawns it per event, \
             so status won't broadcast until it's installed"
        );
    }
    // 1.x loads plugins once at startup; 2.x hot-reloads local TUI plugins,
    // but a restart is the advice that is right on both lines.
    if wrote {
        println!("opencode: restart opencode (or reload plugins) for the bridge to take effect.");
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        OPENCODE_PLUGIN_JS, OPENCODE_PLUGIN_MARKER, OPENCODE_PLUGIN_MARKER_PREFIX, OPENCODE_TUI_PLUGIN_JS,
        OPENCODE_TUI_PLUGIN_MARKER,
    };
    use super::{opencode_config_dir_from, opencode_installed_from};
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

    #[test]
    fn opencode_counts_as_installed_via_binary_config_dir_or_plugin() {
        assert!(opencode_installed_from(true, false, false));
        // A populated ~/.config/opencode with no binary on PATH (Nix/bun-run
        // users) must not make an explicit `setup opencode` silently skip.
        assert!(opencode_installed_from(false, true, false));
        assert!(opencode_installed_from(false, false, true));
        assert!(!opencode_installed_from(false, false, false));
    }

    /// Weld: both embedded bridges carry a marker from the one family the
    /// install path, the doctor, and `run`'s detection key off, and the spawn
    /// contract. A stale `include_str!` target (renamed file, dropped marker,
    /// a `spawnSync` that would freeze the TUI) is caught here rather than at
    /// runtime.
    #[test]
    fn embedded_plugins_carry_marker_and_contract() {
        assert!(OPENCODE_PLUGIN_MARKER.starts_with(OPENCODE_PLUGIN_MARKER_PREFIX));
        assert!(OPENCODE_TUI_PLUGIN_MARKER.starts_with(OPENCODE_PLUGIN_MARKER_PREFIX));
        assert_ne!(OPENCODE_PLUGIN_MARKER, OPENCODE_TUI_PLUGIN_MARKER);
        for (js, marker) in [(OPENCODE_PLUGIN_JS, OPENCODE_PLUGIN_MARKER), (OPENCODE_TUI_PLUGIN_JS, OPENCODE_TUI_PLUGIN_MARKER)] {
            assert!(
                js.lines().next().is_some_and(|l| l.contains(marker)),
                "the vendored plugin must carry the {marker} marker in its header line"
            );
            assert!(js.contains("notify opencode"), "the bridge must spawn `zj-radar notify opencode`");
            assert!(
                !js.contains("spawnSync"),
                "the bridge must never use spawnSync — it runs in opencode's process and would freeze the TUI"
            );
            assert!(js.contains("ZELLIJ"), "the bridge must gate on $ZELLIJ (skip spawn when not under Zellij)");
        }
    }

    /// Weld: the 2.x TUI bridge is a bare file opencode imports from its
    /// plugins dir — no `node_modules` beside it, so it must not `import`
    /// anything (`Plugin.define` is an identity, the object literal is the
    /// contract), and it must be the `{ id, setup }` shape the TUI's module
    /// validator accepts.
    #[test]
    fn embedded_tui_plugin_is_a_self_contained_v2_tui_definition() {
        assert!(
            !OPENCODE_TUI_PLUGIN_JS.lines().any(|l| l.starts_with("import ")),
            "the TUI bridge must not import: nothing resolves beside a bare plugins-dir file"
        );
        assert!(OPENCODE_TUI_PLUGIN_JS.contains("export default {"));
        assert!(OPENCODE_TUI_PLUGIN_JS.contains("id: \"zj-radar\""));
        assert!(OPENCODE_TUI_PLUGIN_JS.contains("setup(ctx)"));
    }

    /// Weld: the 1.x file must load cleanly under 2.x too — as an inert
    /// `{ id, setup }` stub with the 1.x factory on `server` — so a 2.x user
    /// never sees a failed plugin for the file their version doesn't use.
    #[test]
    fn embedded_v1_plugin_is_dual_line() {
        assert!(OPENCODE_PLUGIN_JS.contains("server: ZjRadarPlugin"));
        assert!(OPENCODE_PLUGIN_JS.contains("setup() {}"));
        assert!(OPENCODE_PLUGIN_JS.contains("id: \"zj-radar-server\""));
    }
}
