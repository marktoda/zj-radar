use super::*;

use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) fn codex_config_path() -> Option<PathBuf> {
    codex_home_dir().map(|d| d.join("config.toml"))
}

pub(crate) fn codex_hooks_path() -> Option<PathBuf> {
    codex_home_dir().map(|d| d.join("hooks.json"))
}

/// Read Codex's hooks.json for producer *detection* (`run`'s advisory, `setup
/// zellij`'s epilogue hint, `--check`). Routed through `codex_hooks_path` so
/// `$CODEX_HOME` is honored on the read side exactly as `setup codex` honors
/// it on the write side — a hand-rolled `~/.codex` probe here would tell a
/// `CODEX_HOME` user their correctly-installed hooks are missing.
pub(crate) fn codex_hooks_text() -> Option<String> {
    codex_hooks_path().and_then(|p| std::fs::read_to_string(p).ok())
}

/// Read Codex's config.toml for producer detection — the `--legacy-notify`
/// route's evidence, `$CODEX_HOME`-aware like [`codex_hooks_text`].
pub(crate) fn codex_config_text() -> Option<String> {
    codex_config_path().and_then(|p| std::fs::read_to_string(p).ok())
}

/// The text of a file `setup codex` is about to edit: absent reads as empty
/// (a fresh install), but a present file we can't read (permissions, non-UTF8,
/// a directory) is refused — treating it as empty would write our wiring over
/// the user's config.
fn read_for_edit(path: &std::path::Path) -> Result<String, String> {
    match super::vendored::read_existing(path) {
        super::vendored::Existing::Absent => Ok(String::new()),
        super::vendored::Existing::Text(t) => Ok(t),
        super::vendored::Existing::Unreadable(e) => {
            Err(format!("refused — {} could not be read ({e})", path.display()))
        }
    }
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
    home.filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".codex"))
}

/// Is Codex present? The binary on PATH, or its config/hooks already on disk.
/// Gates both the codex install and the bare doctor.
pub(crate) fn codex_installed(codex_on_path: bool) -> bool {
    codex_on_path || codex_config_path().is_some_and(|p| p.exists()) || codex_hooks_path().is_some_and(|p| p.exists())
}

pub(crate) fn setup_codex(uninstall: bool, opts: CodexSetupOpts) {
    if codex_home_dir().is_none() {
        crate::exit::fail_report("codex", "skipped — set $HOME or $CODEX_HOME so the Codex config dir can be resolved");
        return;
    }
    if opts.legacy_notify {
        setup_codex_notify(uninstall, opts.dry_run, opts.yes, opts.force, opts.is_tty);
    } else {
        setup_codex_hooks(uninstall, opts.dry_run, opts.yes, opts.is_tty);
    }
}

fn setup_codex_hooks(uninstall: bool, dry_run: bool, yes: bool, is_tty: bool) {
    // `setup_codex` already refused when no home resolves, so this is Some.
    let Some(path) = codex_hooks_path() else { return };
    let codex_on_path = which("codex");
    if !uninstall && !codex_installed(codex_on_path) {
        println!("codex: skipped (binary/config not found)");
        return;
    }
    let existing = match read_for_edit(&path) {
        Ok(t) => t,
        Err(e) => return crate::exit::fail_report("codex", e),
    };
    let env = CodexEnv {
        codex_on_path,
        zj_radar_on_path: which("zj-radar"),
        config_text: codex_config_path().and_then(|p| std::fs::read_to_string(p).ok()),
        hooks_text: Some(existing.clone()),
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
            if !confirm_and_write("codex", &path, &new, yes, is_tty, &prompt, || Ok(())) {
                return;
            }
            println!("codex: hooks {} ({})", if uninstall { "removed" } else { "installed" }, path.display());
            if !uninstall {
                print_codex_hook_guidance(&facts);
            }
        }
    }
}

fn setup_codex_notify(uninstall: bool, dry_run: bool, yes: bool, force: bool, is_tty: bool) {
    // `setup_codex` already refused when no home resolves, so this is Some.
    let Some(path) = codex_config_path() else { return };
    if !uninstall && !codex_installed(which("codex")) {
        println!("codex: skipped (binary/config not found)");
        return;
    }
    let existing = match read_for_edit(&path) {
        Ok(t) => t,
        Err(e) => return crate::exit::fail_report("codex", e),
    };
    // The `.bak` is the user's only copy of a notifier `--force` replaced (the
    // vendored-bridge rule, `vendored::write_bridge`): an uninstall puts it
    // back into the slot, and no write may copy our line over it.
    let bak = path_with_suffix(&path, BACKUP_SUFFIX);
    let bak_notify = std::fs::read_to_string(&bak).ok().as_deref().and_then(detect::codex_foreign_notify);
    let restoring = uninstall && bak_notify.is_some();
    let edit = match (&bak_notify, uninstall) {
        (Some(item), true) => restore_codex_notify(&existing, item),
        _ => edit_codex(&existing, !uninstall, force),
    };
    let Some(outcome) = edit_or_report("codex", edit) else {
        return;
    };
    match outcome {
        Outcome::Unchanged if uninstall => println!("codex: legacy notify already removed ({})", path.display()),
        Outcome::Unchanged => println!("codex: legacy notify already up to date ({})", path.display()),
        Outcome::Conflict => {
            crate::exit::fail_report(
                "codex",
                format!(
                    "{} already has a different `notify` program. Refusing to overwrite it.\n\
                     Re-run with --legacy-notify --force to replace it, or use hook setup without --legacy-notify.",
                    path.display()
                ),
            );
        }
        Outcome::Changed(new) => {
            if dry_run {
                println!("--- {} (dry-run) ---\n{new}", path.display());
                return;
            }
            let prompt = format!("Write {}?", path.display());
            if !confirm(&prompt, yes, is_tty) {
                println!("codex: skipped (declined)");
                return;
            }
            // Replacing our slot while the `.bak` holds the user's notifier:
            // skip the backup rather than copy our line over their only copy.
            let replacing_ours = detect::codex_config_notify_is_ours(&existing);
            let written = if replacing_ours && bak_notify.is_some() {
                crate::fsutil::atomic_write(&path, new.as_bytes())
            } else {
                backup_then_write(&path, &new)
            };
            if let Err(e) = written {
                crate::exit::fail_report("codex", format!("write failed — {e}"));
                return;
            }
            let verb = match (uninstall, restoring) {
                (true, true) => "removed — your previous notify restored from",
                (true, false) => "removed",
                (false, _) => "installed",
            };
            if restoring {
                println!("codex: legacy notify {verb} {} ({})", bak.display(), path.display());
            } else {
                println!("codex: legacy notify {verb} ({})", path.display());
            }
        }
    }
}

fn print_codex_hook_guidance(facts: &CodexFacts) {
    if matches!(facts.hooks_feature, CodexHooksFeature::Disabled) {
        if let Some(path) = codex_config_path() {
            eprintln!("codex: warning — hooks appear disabled in {} (`[features].hooks = false`)", path.display());
        }
    }
    println!("codex: {CODEX_HOOK_TRUST_ADVICE}.");
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
        assert_eq!(codex_home_from(Some(os("/x/codex")), Some(os("/home/u"))), Some(PathBuf::from("/x/codex")),);
    }

    #[test]
    fn codex_home_falls_back_to_home_dot_codex() {
        assert_eq!(codex_home_from(None, Some(os("/home/u"))), Some(PathBuf::from("/home/u/.codex")),);
    }

    #[test]
    fn codex_home_is_none_when_neither_resolves() {
        // Neither set → None (never a relative `.codex` in the CWD).
        assert_eq!(codex_home_from(None, None), None);
        // Empty strings are treated as unset, not as the root path.
        assert_eq!(codex_home_from(Some(OsString::new()), Some(OsString::new())), None);
        assert_eq!(codex_home_from(None, Some(OsString::new())), None);
        // An empty CODEX_HOME still lets a real HOME win.
        assert_eq!(codex_home_from(Some(OsString::new()), Some(os("/home/u"))), Some(PathBuf::from("/home/u/.codex")),);
    }
}
