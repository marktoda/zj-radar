//! `zj-radar setup claude` — wire the Claude Code producer through Claude
//! Code's own plugin marketplace.
//!
//! Symmetry with `setup codex` stops at the mechanism: Codex has no plugin
//! marketplace, so we edit `hooks.json` directly; Claude Code has one, so we
//! drive the `claude plugin` CLI and never touch its files. The marketplace
//! owns the plugin's update channel, and a second hand-written wiring in
//! `settings.json` would double-fire every hook event.

use super::*;

/// The plugin name as it appears installed (and in `installed_plugins.json`,
/// where `crate::producers` detects it).
pub(crate) const CLAUDE_PLUGIN: &str = "zj-radar-claude";

/// The marketplace's NAME once added — Claude Code names it after the repo's
/// basename, so it is derived from the slug `marketplace add` takes
/// (`repo_slug()`: Cargo's `repository`, `ZJ_RADAR_REPO` overriding — the
/// same fork-follows knob as `--download`). The one derivation behind the
/// qualified plugin id (`zj-radar-claude@<name>`) and the uninstall
/// epilogue's `marketplace remove <name>`, so the three can't drift on a fork.
/// Uses the crate's empty-segment-guarded [`crate::agents::basename`], so a
/// degenerate slug (trailing slash, empty) falls back to the slug whole
/// rather than yielding an empty name (`zj-radar-claude@`).
fn claude_marketplace_name(repo_slug: &str) -> &str {
    crate::agents::basename(repo_slug).unwrap_or(repo_slug)
}

/// The qualified plugin id (`zj-radar-claude@<marketplace>`) for the current
/// `repo_slug()` — what `claude plugin install` and `/plugin update` take.
pub(crate) fn claude_plugin_id() -> String {
    format!("{CLAUDE_PLUGIN}@{}", claude_marketplace_name(&repo_slug()))
}

/// Read Claude Code's installed-plugins manifest
/// (`<config dir>/plugins/installed_plugins.json`) for producer *detection* —
/// the same three consumers as [`codex_hooks_text`], and the same drift class
/// it guards against: one reader, so `run`'s advisory, `setup zellij`'s
/// epilogue hint, and `--check` can never probe different paths.
pub(crate) fn claude_installed_plugins_text() -> Option<String> {
    claude_config_dir_from(std::env::var_os("CLAUDE_CONFIG_DIR"), dirs::home_dir())
        .and_then(|d| std::fs::read_to_string(d.join("plugins").join("installed_plugins.json")).ok())
}

/// Resolve Claude Code's config dir: `$CLAUDE_CONFIG_DIR` wins (when
/// non-empty), else `<home>/.claude`. A hard-coded `~/.claude` told a
/// `CLAUDE_CONFIG_DIR` user their installed plugin was missing. Pure (env
/// passed in) so the precedence is unit-tested, like `codex_home_from`.
fn claude_config_dir_from(config_dir: Option<std::ffi::OsString>, home: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    if let Some(d) = config_dir.filter(|d| !d.is_empty()) {
        return Some(std::path::PathBuf::from(d));
    }
    home.filter(|h| !h.as_os_str().is_empty()).map(|h| h.join(".claude"))
}

pub(crate) fn setup_claude(uninstall: bool, dry_run: bool, yes: bool, is_tty: bool) {
    let wired = crate::producers::ProducerTexts::read().is_wired(crate::agents::Agent::Claude);
    if uninstall {
        uninstall_claude(wired, dry_run, yes, is_tty);
    } else {
        install_claude(wired, dry_run, yes, is_tty);
    }
}

fn install_claude(wired: bool, dry_run: bool, yes: bool, is_tty: bool) {
    if wired {
        println!("claude: already wired ({CLAUDE_PLUGIN} plugin installed)");
        return;
    }
    if !which("claude") {
        // Mirrors codex's "skipped (binary/config not found)": a machine
        // without the agent is not an error — bare `setup` reaches here for
        // every detected-or-not agent.
        println!("claude: skipped (binary not found)");
        return;
    }
    let marketplace = repo_slug();
    let plugin_id = claude_plugin_id();
    if dry_run {
        println!("claude: would run `claude plugin marketplace add {marketplace}` (dry-run)");
        println!("claude: would run `claude plugin install {plugin_id}` (dry-run)");
        return;
    }
    if !confirm(
        &format!(
            "Install the {CLAUDE_PLUGIN} producer via Claude Code's plugin marketplace \
             (adds the {marketplace} marketplace)?"
        ),
        yes,
        is_tty,
    ) {
        println!("claude: skipped (declined)");
        return;
    }
    // Adding an already-configured marketplace may fail depending on the
    // Claude Code version — not worth parsing; the install below is the step
    // whose failure actually means something.
    if let Err(e) = run_claude(&["plugin", "marketplace", "add", &marketplace]) {
        eprintln!("claude: marketplace add did not succeed ({e}) — continuing, it may already be configured");
    }
    if let Err(e) = run_claude(&["plugin", "install", &plugin_id]) {
        crate::exit::fail_report("claude", format!("plugin install failed — {e}"));
        return;
    }
    println!(
        "claude: installed {CLAUDE_PLUGIN} via the plugin marketplace — \
         new Claude Code sessions pick it up"
    );
}

fn uninstall_claude(wired: bool, dry_run: bool, yes: bool, is_tty: bool) {
    if !wired {
        println!("claude: already removed ({CLAUDE_PLUGIN} plugin not installed)");
        return;
    }
    // The qualified id, exactly as `install_claude` installed it (and as
    // installed_plugins.json keys it): a bare name is ambiguous when another
    // marketplace ships a plugin of the same name.
    let plugin_id = claude_plugin_id();
    if dry_run {
        println!("claude: would run `claude plugin uninstall {plugin_id}` (dry-run)");
        return;
    }
    if !which("claude") {
        crate::exit::fail_report(
            "claude",
            "claude binary not found on PATH — remove the plugin from inside \
             Claude Code (`/plugin`) instead",
        );
        return;
    }
    if !confirm(&format!("Uninstall the {CLAUDE_PLUGIN} plugin via `claude plugin uninstall`?"), yes, is_tty) {
        println!("claude: skipped (declined)");
        return;
    }
    if let Err(e) = run_claude(&["plugin", "uninstall", &plugin_id]) {
        crate::exit::fail_report("claude", format!("plugin uninstall failed — {e}"));
        return;
    }
    println!(
        "claude: removed the {CLAUDE_PLUGIN} plugin (marketplace entry left in place — \
         remove with `claude plugin marketplace remove {}`)",
        claude_marketplace_name(&repo_slug())
    );
}

/// Run `claude <args>` inheriting stdio, so the plugin CLI's own progress and
/// errors reach the user unfiltered.
fn run_claude(args: &[&str]) -> Result<(), String> {
    match std::process::Command::new("claude").args(args).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("`claude {}` exited with {s}", args.join(" "))),
        Err(e) => Err(format!("could not run `claude` — {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_name_is_the_slug_basename_so_forks_agree() {
        // The name Claude Code assigns to an added marketplace is the repo's
        // basename. Because the plugin id and the uninstall epilogue both call
        // this on the same slug `marketplace add` takes, the three surfaces
        // agree structurally — including under a `ZJ_RADAR_REPO` override.
        assert_eq!(claude_marketplace_name("marktoda/zj-radar"), "zj-radar");
        assert_eq!(claude_marketplace_name("fork-owner/zj-radar-fork"), "zj-radar-fork");
        // Degenerate slug without a slash: use it whole rather than panic.
        assert_eq!(claude_marketplace_name("zj-radar"), "zj-radar");
        // Trailing slash must not yield an empty name (`zj-radar-claude@` and
        // a dangling `marketplace remove `): fall back to the slug whole.
        assert_eq!(claude_marketplace_name("marktoda/zj-radar/"), "marktoda/zj-radar/");
    }

    #[test]
    fn claude_config_dir_prefers_the_env_override_over_home() {
        use std::ffi::OsString;
        use std::path::PathBuf;
        let home = || Some(PathBuf::from("/home/u"));
        assert_eq!(claude_config_dir_from(Some(OsString::from("/x/claude")), home()), Some(PathBuf::from("/x/claude")));
        assert_eq!(claude_config_dir_from(None, home()), Some(PathBuf::from("/home/u/.claude")));
        // Empty is unset, not the root path.
        assert_eq!(claude_config_dir_from(Some(OsString::new()), home()), Some(PathBuf::from("/home/u/.claude")));
        assert_eq!(claude_config_dir_from(None, None), None);
        assert_eq!(claude_config_dir_from(Some(OsString::from("/x/claude")), None), Some(PathBuf::from("/x/claude")));
    }
}
