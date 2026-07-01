use super::*;

use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) fn codex_config_path() -> Option<PathBuf> {
    codex_home_dir().map(|d| d.join("config.toml"))
}

pub(crate) fn codex_hooks_path() -> Option<PathBuf> {
    codex_home_dir().map(|d| d.join("hooks.json"))
}

fn codex_home_dir() -> Option<PathBuf> {
    codex_home_from(std::env::var_os("CODEX_HOME"), std::env::var_os("HOME"))
}

/// Resolve Codex's config home: `$CODEX_HOME` wins, else `$HOME/.codex`. `None`
/// when neither is set (or is empty) — callers that *write* refuse rather than
/// invent a path. A bare `unwrap_or_default` here used to fall back to an empty
/// path, silently targeting a relative `.codex` in the process's CWD. Pure (env
/// passed in) so the precedence is unit-tested without touching process env.
fn codex_home_from(codex_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(h) = codex_home.filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(h));
    }
    home.filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".codex"))
}

fn codex_installed(codex_on_path: bool) -> bool {
    codex_on_path
        || codex_config_path().is_some_and(|p| p.exists())
        || codex_hooks_path().is_some_and(|p| p.exists())
}

pub(crate) fn setup_codex(uninstall: bool, opts: CodexSetupOpts) {
    if codex_home_dir().is_none() {
        eprintln!(
            "codex: skipped — set $HOME or $CODEX_HOME so the Codex config dir can be resolved"
        );
        return;
    }
    if opts.legacy_notify {
        setup_codex_notify(uninstall, opts.dry_run, opts.yes, opts.force);
    } else {
        setup_codex_hooks(uninstall, opts.dry_run, opts.yes);
    }
}

fn setup_codex_hooks(uninstall: bool, dry_run: bool, yes: bool) {
    // `setup_codex` already refused when no home resolves, so this is Some.
    let Some(path) = codex_hooks_path() else { return };
    let codex_on_path = which("codex");
    if !uninstall && !codex_installed(codex_on_path) {
        println!("codex: skipped (binary/config not found)");
        return;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let env = CodexEnv {
        codex_on_path,
        zj_radar_on_path: which("zj-radar"),
        config_text:      codex_config_path().and_then(|p| std::fs::read_to_string(p).ok()),
        hooks_text:       Some(existing.clone()),
    };
    let facts = analyze_codex(&env);
    let Some(outcome) = edit_or_report("codex", edit_codex_hooks(&existing, !uninstall)) else {
        return;
    };
    match outcome {
        Outcome::Unchanged if uninstall => {
            println!("codex: hooks already removed ({})", path.display())
        }
        Outcome::Unchanged => {
            println!("codex: hooks already up to date ({})", path.display());
            print_codex_hook_guidance(&facts);
        }
        Outcome::Conflict => unreachable!("codex hooks editor has no conflict outcome"),
        Outcome::Changed(new) => {
            if dry_run {
                println!("--- {} (dry-run) ---\n{new}", path.display());
                if !uninstall {
                    print_codex_hook_guidance(&facts);
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
                print_codex_hook_guidance(&facts);
            }
        }
    }
}

fn setup_codex_notify(uninstall: bool, dry_run: bool, yes: bool, force: bool) {
    // `setup_codex` already refused when no home resolves, so this is Some.
    let Some(path) = codex_config_path() else { return };
    if !uninstall && !codex_installed(which("codex")) {
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

fn print_codex_hook_guidance(facts: &CodexFacts) {
    if matches!(facts.hooks_feature, CodexHooksFeature::Disabled) {
        if let Some(path) = codex_config_path() {
            eprintln!(
                "codex: warning — hooks appear disabled in {} (`[features].hooks = false`)",
                path.display()
            );
        }
    }
    println!("codex: run `/hooks` in Codex to review and trust the zj-radar command hook.");
}

#[cfg(test)]
mod tests {
    use super::codex_home_from;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn codex_home_prefers_codex_home_over_home() {
        assert_eq!(
            codex_home_from(Some(os("/x/codex")), Some(os("/home/u"))),
            Some(PathBuf::from("/x/codex")),
        );
    }

    #[test]
    fn codex_home_falls_back_to_home_dot_codex() {
        assert_eq!(
            codex_home_from(None, Some(os("/home/u"))),
            Some(PathBuf::from("/home/u/.codex")),
        );
    }

    #[test]
    fn codex_home_is_none_when_neither_resolves() {
        // Neither set → None (never a relative `.codex` in the CWD).
        assert_eq!(codex_home_from(None, None), None);
        // Empty strings are treated as unset, not as the root path.
        assert_eq!(codex_home_from(Some(OsString::new()), Some(OsString::new())), None);
        assert_eq!(codex_home_from(None, Some(OsString::new())), None);
        // An empty CODEX_HOME still lets a real HOME win.
        assert_eq!(
            codex_home_from(Some(OsString::new()), Some(os("/home/u"))),
            Some(PathBuf::from("/home/u/.codex")),
        );
    }
}
