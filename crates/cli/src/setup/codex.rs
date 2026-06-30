use super::*;

use std::path::PathBuf;

pub(crate) fn codex_config_path() -> PathBuf {
    codex_home_dir().join("config.toml")
}

pub(crate) fn codex_hooks_path() -> PathBuf {
    codex_home_dir().join("hooks.json")
}

fn codex_home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(home);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".codex")
}

fn codex_installed() -> bool {
    which("codex") || codex_config_path().exists() || codex_hooks_path().exists()
}

pub(crate) fn setup_codex(uninstall: bool, opts: CodexSetupOpts) {
    if opts.legacy_notify {
        setup_codex_notify(uninstall, opts.dry_run, opts.yes, opts.force);
    } else {
        setup_codex_hooks(uninstall, opts.dry_run, opts.yes);
    }
}

fn setup_codex_hooks(uninstall: bool, dry_run: bool, yes: bool) {
    let path = codex_hooks_path();
    if !uninstall && !codex_installed() {
        println!("codex: skipped (binary/config not found)");
        return;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let Some(outcome) = edit_or_report("codex", edit_codex_hooks(&existing, !uninstall)) else {
        return;
    };
    match outcome {
        Outcome::Unchanged if uninstall => {
            println!("codex: hooks already removed ({})", path.display())
        }
        Outcome::Unchanged => {
            println!("codex: hooks already up to date ({})", path.display());
            print_codex_hook_guidance();
        }
        Outcome::Conflict => unreachable!("codex hooks editor has no conflict outcome"),
        Outcome::Changed(new) => {
            if dry_run {
                println!("--- {} (dry-run) ---\n{new}", path.display());
                if !uninstall {
                    print_codex_hook_guidance();
                }
                return;
            }
            let prompt = format!("Write {}?", path.display());
            if !confirm_and_write("codex", &path, &new, yes, &prompt, || Ok(())) {
                return;
            }
            println!(
                "codex: hooks {} ({})",
                if uninstall { "removed" } else { "installed" },
                path.display()
            );
            if !uninstall {
                print_codex_hook_guidance();
            }
        }
    }
}

fn setup_codex_notify(uninstall: bool, dry_run: bool, yes: bool, force: bool) {
    let path = codex_config_path();
    if !uninstall && !codex_installed() {
        println!("codex: skipped (binary/config not found)");
        return;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let Some(outcome) = edit_or_report("codex", edit_codex(&existing, !uninstall, force)) else {
        return;
    };
    match outcome {
        Outcome::Unchanged => println!(
            "codex: legacy notify already up to date ({})",
            path.display()
        ),
        Outcome::Conflict => {
            eprintln!(
                "codex: {} already has a different `notify` program. Refusing to overwrite it.\n\
                 Re-run with --legacy-notify --force to replace it, or use hook setup without --legacy-notify.",
                path.display()
            );
        }
        Outcome::Changed(new) => {
            if dry_run {
                println!("--- {} (dry-run) ---\n{new}", path.display());
                return;
            }
            let prompt = format!("Write {}?", path.display());
            if !confirm_and_write("codex", &path, &new, yes, &prompt, || Ok(())) {
                return;
            }
            println!(
                "codex: legacy notify {} ({})",
                if uninstall { "removed" } else { "installed" },
                path.display()
            );
        }
    }
}

fn print_codex_hook_guidance() {
    if codex_hooks_disabled() {
        eprintln!(
            "codex: warning — hooks appear disabled in {} (`[features].hooks = false`)",
            codex_config_path().display()
        );
    }
    println!("codex: run `/hooks` in Codex to review and trust the zj-radar command hook.");
}

fn codex_hooks_disabled() -> bool {
    let env = CodexEnv {
        codex_on_path:    false,
        zj_radar_on_path: false,
        config_text:      std::fs::read_to_string(codex_config_path()).ok(),
        hooks_text:       None,
    };
    matches!(analyze_codex(&env).hooks_feature, CodexHooksFeature::Disabled)
}
