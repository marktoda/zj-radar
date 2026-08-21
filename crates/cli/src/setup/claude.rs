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
/// where [`crate::run::claude_producer_wired`] detects it).
pub(crate) const CLAUDE_PLUGIN: &str = "zj-radar-claude";

/// The marketplace's NAME once added — Claude Code names it after the repo's
/// basename, so it is derived from the slug `marketplace add` takes
/// (`repo_slug()`: Cargo's `repository`, `ZJ_RADAR_REPO` overriding — the
/// same fork-follows knob as `--download`). The one derivation behind the
/// qualified plugin id (`zj-radar-claude@<name>`) and the uninstall
/// epilogue's `marketplace remove <name>`, so the three can't drift on a fork.
fn claude_marketplace_name(repo_slug: &str) -> &str {
    repo_slug.rsplit('/').next().unwrap_or(repo_slug)
}

/// Read Claude Code's installed-plugins manifest
/// (`~/.claude/plugins/installed_plugins.json`) for producer *detection* —
/// the same three consumers as [`codex_hooks_text`], and the same drift class
/// it guards against: one reader, so `run`'s advisory, `setup zellij`'s
/// epilogue hint, and `--check` can never probe different paths.
pub(crate) fn claude_installed_plugins_text() -> Option<String> {
    dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".claude/plugins/installed_plugins.json")).ok())
}

pub(crate) fn setup_claude(uninstall: bool, dry_run: bool, yes: bool) {
    use std::io::IsTerminal;
    let wired = crate::run::claude_producer_wired(claude_installed_plugins_text().as_deref());
    // Tty-ness resolved once at the boundary (the `inject_mode` pattern);
    // `confirm` below takes it as a parameter.
    let is_tty = std::io::stdin().is_terminal();
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
    let plugin_id = format!("{CLAUDE_PLUGIN}@{}", claude_marketplace_name(&marketplace));
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
    if dry_run {
        println!("claude: would run `claude plugin uninstall {CLAUDE_PLUGIN}` (dry-run)");
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
    if let Err(e) = run_claude(&["plugin", "uninstall", CLAUDE_PLUGIN]) {
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
    }
}
