use super::*;

use std::path::{Path, PathBuf};

/// Decision about how to handle layout injection for a given invocation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InjectMode {
    /// Inject without prompting (`--inject` was passed).
    Inject,
    /// Ask the user interactively (default N — no mutation without explicit y).
    Prompt,
    /// Print the tailored snippet and skip injection. The safe non-mutating
    /// default when `--yes` is in effect or when stdin is not a tty.
    Snippet,
}

/// Pure decision: given the CLI flags and whether stdin is a tty, decide how
/// the layout injection step should behave. The rules are:
///
/// 1. `--inject` → `Inject` (unconditional explicit consent).
/// 2. `--yes` → `Snippet`  (take the safe default; never mutate silently).
/// 3. Not a tty → `Snippet` (no way to ask).
/// 4. Otherwise → `Prompt`  (interactive).
pub(crate) fn inject_mode(inject_flag: bool, yes: bool, is_tty: bool) -> InjectMode {
    if inject_flag {
        return InjectMode::Inject;
    }
    if yes || !is_tty {
        return InjectMode::Snippet;
    }
    InjectMode::Prompt
}

/// Pure: the argument vector for `zellij plugin --floating --width 90 --height
/// 28 file:<wasm_path>`. Unit-tested so the exec call stays thin.
pub(crate) fn grant_args(wasm_path: &Path) -> Vec<String> {
    vec![
        "plugin".to_string(),
        "--floating".to_string(),
        "--width".to_string(),
        "90".to_string(),
        "--height".to_string(),
        "28".to_string(),
        format!("file:{}", wasm_path.display()),
    ]
}

/// Exec `zellij plugin --floating … file:<wasm_dest>` for the one-time
/// permission grant. Reports the error but does not exit — callers may choose.
pub(crate) fn run_grant(config_dir: &Path) {
    use std::process::Command;
    let wasm_dest = zellij_wasm_dest(config_dir);
    let args = grant_args(&wasm_dest);
    match Command::new("zellij").args(&args).status() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!(
                "zj-radar: zellij plugin exited with {status}; \
                 try running: zellij {}",
                args.join(" ")
            );
        }
        Err(e) => {
            eprintln!(
                "zj-radar: failed to launch zellij for grant — {e}; \
                 try running: zellij {}",
                args.join(" ")
            );
        }
    }
}

pub(crate) fn zellij_config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZELLIJ_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config").join("zellij")
}

pub(crate) fn zellij_config_path(config_dir: &Path) -> PathBuf {
    config_dir.join("config.kdl")
}

pub(crate) fn zellij_wasm_dest(config_dir: &Path) -> PathBuf {
    config_dir.join("plugins").join("zj_radar.wasm")
}

fn zellij_plugin_location(path: &Path) -> String {
    let display_path = if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        path.strip_prefix(&home)
            .ok()
            .map(|rel| format!("~/{}", rel.display()))
            .unwrap_or_else(|| path.display().to_string())
    } else {
        path.display().to_string()
    };
    format!("file:{display_path}")
}

/// Returns `true` when `path` is a symlink — the hallmark of a Nix / home-manager
/// managed file that we must not overwrite. Uses `symlink_metadata` so the query
/// does not follow the link (a broken symlink still returns `true`).
pub(crate) fn config_is_managed(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

pub(crate) fn setup_zellij(uninstall: bool, opts: ZellijSetupOpts<'_>) {
    let (wasm, download): (Option<&Path>, bool) = match &opts.wasm_source {
        WasmSource::Path(p) => (Some(p.as_path()), false),
        WasmSource::Download => (None, true),
        WasmSource::None    => (None, false),
    };
    let dry_run     = opts.dry_run;
    let yes         = opts.yes;
    let force       = opts.force;
    let inject_flag = opts.inject;
    let layout_name = opts.layout;
    let config_dir = zellij_config_dir();
    let config_path = zellij_config_path(&config_dir);
    let wasm_dest = zellij_wasm_dest(&config_dir);
    let location = zellij_plugin_location(&wasm_dest);

    // Resolve the target layout path up front (needed whether or not a managed
    // config short-circuits): `--layout <name>` → `<config_dir>/layouts/<name>.kdl`,
    // else `<config_dir>/layouts/default.kdl`.
    let layout_path = config_dir
        .join("layouts")
        .join(format!("{}.kdl", layout_name.unwrap_or("default")));

    // One derivation, shared with `check`: read current state into Facts. The
    // config text is reused below for the `edit_zellij` splice.
    let config_text = std::fs::read_to_string(&config_path).ok();
    let codex_hooks_text = dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".codex/hooks.json")).ok());
    let installed_plugins_text = dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".claude/plugins/installed_plugins.json")).ok());
    let facts = analyze_zellij(&ZellijEnv {
        config_text:            config_text.clone(),
        layout_text:            None, // install only consults `config_managed`; the layout is read later by the inject flow
        permissions_text:       None,
        codex_hooks_text,
        installed_plugins_text,
        wasm_present:           wasm_dest.is_file(),
        config_managed:         config_is_managed(&config_path),
        wasm_path:              wasm_dest.to_string_lossy().into_owned(),
    });

    // Refuse to touch a managed (symlinked) config.kdl. On install we print the
    // layout snippet for guidance; on uninstall we skip the config rewrite (the
    // alias lives in the user's Nix config, so removal is their Nix concern) but
    // still strip the injected rail from the (separate) layout file. Either way a
    // Nix/home-manager user's managed config is never overwritten.
    if facts.config_managed {
        eprintln!(
            "zellij: config.kdl at {} is a symlink (managed by Nix / home-manager).\n\
             zj-radar will not {} a managed config — {} the plugin alias via\n\
             your Nix config instead. See docs/install.md for the home-manager snippet.",
            config_path.display(),
            if uninstall { "modify" } else { "overwrite" },
            if uninstall { "remove" } else { "add" },
        );
        if uninstall {
            run_layout_uninstall(&layout_path, dry_run);
        } else {
            print_snippet_for(&layout_path);
        }
        return;
    }

    // Resolve the wasm source: an explicit --wasm path, or --download (fetch the
    // wasm matching this CLI's version). `downloaded` outlives the borrow in `src`.
    let downloaded: PathBuf;
    let src: Option<&Path> = if uninstall {
        None
    } else if download {
        match download_wasm(&wasm_download_version()) {
            Ok(path) => {
                downloaded = path;
                Some(downloaded.as_path())
            }
            Err(e) => {
                eprintln!("zellij: refused — {e}");
                return;
            }
        }
    } else {
        wasm
    };

    // When `--inject` is set (or `--yes` is set for a non-mutating snippet) but
    // no wasm source is given, skip the wasm/alias step and go directly to the
    // layout step. This makes `setup zellij --inject` and `setup zellij --yes`
    // usable and testable without a wasm artifact while preserving the existing
    // "refused — pass --wasm" error for bare `setup zellij` invocations.
    let layout_only_install = src.is_none() && !uninstall && (inject_flag || yes);
    if layout_only_install {
        run_layout_inject(&layout_path, inject_flag, yes, dry_run);
        return;
    }
    // `--uninstall` with no wasm/config: layout-only uninstall.
    if uninstall && src.is_none() && !config_path.exists() {
        run_layout_uninstall(&layout_path, dry_run);
        return;
    }

    if !uninstall {
        let Some(src) = src else {
            eprintln!("zellij: refused — pass --wasm <path-to-zj_radar.wasm> or --download");
            return;
        };
        if !src.is_file() {
            eprintln!("zellij: refused — wasm not found at {}", src.display());
            return;
        }
    }

    let existing = config_text.unwrap_or_default();
    let Some(outcome) = edit_or_report("zellij", edit_zellij(&existing, &location, !uninstall, force))
    else {
        return;
    };

    match outcome {
        Outcome::Unchanged if uninstall => {
            println!("zellij: already removed ({})", config_path.display());
            // uninstall: also try to remove the injected rail from the layout.
            run_layout_uninstall(&layout_path, dry_run);
        }
        Outcome::Unchanged => {
            println!(
                "zellij: config already up to date ({})",
                config_path.display()
            );
            // alias already up to date — still offer injection.
            run_layout_inject(&layout_path, inject_flag, yes, dry_run);
            print_grant_hint();
            print_producer_hint_if_needed(&facts);
        }
        Outcome::Conflict => {
            eprintln!(
                "zellij: {} already has an unmanaged `radar` plugin alias. Refusing to overwrite it.\n\
                 Re-run with --force to replace it, or wire zj-radar manually.",
                config_path.display()
            );
        }
        Outcome::Changed(new) => {
            if dry_run {
                if !uninstall {
                    if let Some(src) = src {
                        println!(
                            "zellij: would copy {} -> {}",
                            src.display(),
                            wasm_dest.display()
                        );
                    }
                }
                println!("--- {} (dry-run) ---\n{new}", config_path.display());
                if uninstall {
                    run_layout_uninstall(&layout_path, dry_run);
                } else {
                    run_layout_inject(&layout_path, inject_flag, yes, dry_run);
                }
                return;
            }
            let prompt = if uninstall {
                format!("Update {}?", config_path.display())
            } else {
                format!(
                    "Copy wasm to {} and update {}?",
                    wasm_dest.display(),
                    config_path.display()
                )
            };
            // Pre-write side effect: stage the wasm (mkdir + copy) before the
            // config write, only when installing.
            let copy_wasm = || -> Result<(), String> {
                if uninstall {
                    return Ok(());
                }
                if let Some(parent) = wasm_dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("create plugin dir failed — {e}"))?;
                }
                let src = src.ok_or("refused — pass --wasm <path-to-zj_radar.wasm> or --download")?;
                std::fs::copy(src, &wasm_dest).map_err(|e| format!("wasm copy failed — {e}"))?;
                Ok(())
            };
            if !confirm_and_write("zellij", &config_path, &new, yes, &prompt, copy_wasm) {
                return;
            }
            println!(
                "zellij: {} ({})",
                if uninstall { "removed" } else { "installed" },
                config_path.display()
            );
            if uninstall {
                run_layout_uninstall(&layout_path, dry_run);
            } else {
                println!("zellij: wasm installed at {}", wasm_dest.display());
                run_layout_inject(&layout_path, inject_flag, yes, dry_run);
                print_grant_hint();
                print_producer_hint_if_needed(&facts);
            }
        }
    }
}

fn print_grant_hint() {
    // The rail can't show Zellij's grant prompt legibly (it's a small borderless
    // pane — Zellij #4749). On first launch the user grants by focusing the rail
    // and pressing y; `--grant` offers an explicit floating prompt instead. The
    // turnkey `zj-radar run` handles this automatically. One coherent line — the
    // merge with main's onboarding work otherwise printed two overlapping notes.
    println!(
        "zellij: on first launch, focus the RADAR rail (the left column) and press y to \
         allow access — or run `zj-radar setup zellij --grant` to grant via a floating \
         pane. Zellij asks once, then remembers."
    );
}

/// Emit a producer hint at the tail of `setup zellij` when no producer is wired,
/// per `facts.producer_wired` (derived from Codex hooks + the Claude plugin
/// manifest, same as `run`'s detection — see `analyze_zellij`).
fn print_producer_hint_if_needed(facts: &ZellijFacts) {
    if !facts.producer_wired {
        println!("zellij: {}", crate::run::PRODUCER_HINT);
    }
}

/// Print the "Add the sidebar to a Zellij layout with:" paste snippet, tailored
/// to these layout facts. Single source for the manual-add instruction shared by
/// every snippet-only path (no layout, `--yes`/non-tty, declined prompt).
fn print_paste_snippet(facts: &crate::layout::LayoutFacts) {
    let snippet = crate::layout::tailored_snippet(facts);
    println!("\nAdd the sidebar to a Zellij layout with:\n\n{snippet}");
}

/// Print the tailored snippet for a given layout path (empty string → default facts).
fn print_snippet_for(layout_path: &Path) {
    let text = std::fs::read_to_string(layout_path).unwrap_or_default();
    print_paste_snippet(&crate::layout::analyze(&text));
}

/// Handle layout injection after the alias step. Reads `layout_path`, decides
/// the mode, and either injects (writing a `.zj-radar.bak` backup first) or
/// prints the tailored snippet. A missing layout → snippet only (safe fallback).
fn run_layout_inject(layout_path: &Path, inject_flag: bool, yes: bool, dry_run: bool) {
    use std::io::IsTerminal;
    let is_tty = std::io::stdin().is_terminal();
    let mode = inject_mode(inject_flag, yes, is_tty);

    let text = match std::fs::read_to_string(layout_path) {
        Ok(t) => t,
        Err(_) => {
            // Layout not found — just print the snippet, no failure.
            let facts = crate::layout::analyze("");
            let snippet = crate::layout::tailored_snippet(&facts);
            println!(
                "zellij: layout not found at {} — add the rail manually:\n\n{snippet}",
                layout_path.display()
            );
            return;
        }
    };

    let facts = crate::layout::analyze(&text);

    // Already injected → idempotent no-op for Inject/Prompt; snippet still accurate.
    if facts.has_rail {
        println!("zellij: layout already has the rail ({})", layout_path.display());
        return;
    }

    match mode {
        InjectMode::Snippet => {
            // --yes or non-tty: print snippet, never mutate.
            print_paste_snippet(&facts);
        }
        InjectMode::Prompt => {
            let prompt = format!("Inject the rail into {}?", layout_path.display());
            if !confirm(&prompt) {
                print_paste_snippet(&facts);
                return;
            }
            do_inject(layout_path, &text, &facts, dry_run);
        }
        InjectMode::Inject => {
            do_inject(layout_path, &text, &facts, dry_run);
        }
    }
}

/// Perform the actual inject: call `layout::inject`, write backup + new text.
/// On `Refusal`, print the reason + tailored snippet (fail-closed).
fn do_inject(layout_path: &Path, text: &str, facts: &crate::layout::LayoutFacts, dry_run: bool) {
    match crate::layout::inject(text, facts) {
        Ok(new_text) => {
            if dry_run {
                println!(
                    "zellij: would inject rail into {} (dry-run)\n--- layout (dry-run) ---\n{new_text}",
                    layout_path.display()
                );
                return;
            }
            // Back up then atomically write (shared setup helper).
            match write_atomic(layout_path, &new_text) {
                Ok(()) => println!(
                    "zellij: rail injected into {} (backup: {}.zj-radar.bak)",
                    layout_path.display(),
                    layout_path.display()
                ),
                Err(e) => eprintln!("zellij: write failed — {e}"),
            }
        }
        Err(crate::layout::Refusal::Unparseable(msg)) => {
            eprintln!("zellij: layout could not be parsed — {msg}");
            eprintln!("        Add the rail manually using the snippet below.");
            let snippet = crate::layout::tailored_snippet(facts);
            println!("\n{snippet}");
        }
        Err(crate::layout::Refusal::Unrecognized(msg)) => {
            eprintln!("zellij: layout shape not recognized — {msg}");
            eprintln!("        Add the rail manually using the snippet below.");
            let snippet = crate::layout::tailored_snippet(facts);
            println!("\n{snippet}");
        }
    }
}

/// Handle `--uninstall` for the layout: strip the injected rail if present.
fn run_layout_uninstall(layout_path: &Path, dry_run: bool) {
    let text = match std::fs::read_to_string(layout_path) {
        Ok(t) => t,
        Err(_) => return, // layout not found — nothing to uninstall
    };
    match crate::layout::uninstall(&text) {
        None => {
            // no injected rail present — nothing to do
        }
        Some(new_text) => {
            if dry_run {
                println!(
                    "zellij: would remove rail from {} (dry-run)\n--- layout (dry-run) ---\n{new_text}",
                    layout_path.display()
                );
                return;
            }
            match write_atomic(layout_path, &new_text) {
                Ok(()) => println!(
                    "zellij: rail removed from {} (backup: {}.zj-radar.bak)",
                    layout_path.display(),
                    layout_path.display()
                ),
                Err(e) => eprintln!("zellij: write failed — {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── inject_mode decision tests ───────────────────────────────────────────

    #[test]
    fn inject_flag_forces_inject() {
        assert_eq!(inject_mode(true, false, false), InjectMode::Inject);
        assert_eq!(inject_mode(true, false, true), InjectMode::Inject);
        assert_eq!(inject_mode(true, true, false), InjectMode::Inject);
        assert_eq!(inject_mode(true, true, true), InjectMode::Inject);
    }

    #[test]
    fn yes_takes_safe_default_snippet() {
        // --yes without --inject → Snippet regardless of tty
        assert_eq!(inject_mode(false, true, true),  InjectMode::Snippet);
        assert_eq!(inject_mode(false, true, false), InjectMode::Snippet);
    }

    #[test]
    fn non_tty_takes_safe_default_snippet() {
        // non-tty without --inject or --yes → Snippet
        assert_eq!(inject_mode(false, false, false), InjectMode::Snippet);
    }

    #[test]
    fn prompt_when_interactive() {
        // interactive tty, no --inject, no --yes → Prompt
        assert_eq!(inject_mode(false, false, true), InjectMode::Prompt);
    }

    // ── grant_args tests ─────────────────────────────────────────────────────

    #[test]
    fn grant_args_produces_exact_zellij_plugin_command() {
        let path = std::path::Path::new("/home/user/.config/zellij/plugins/zj_radar.wasm");
        assert_eq!(
            grant_args(path),
            vec![
                "plugin",
                "--floating",
                "--width",
                "90",
                "--height",
                "28",
                "file:/home/user/.config/zellij/plugins/zj_radar.wasm",
            ]
        );
    }

    #[test]
    #[cfg(unix)]
    fn detects_symlinked_config_as_managed() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.kdl");
        std::fs::write(&real, "").unwrap();
        let link = dir.path().join("config.kdl");
        symlink(&real, &link).unwrap();
        assert!(config_is_managed(&link), "symlink should be managed");
        assert!(!config_is_managed(&real), "regular file should not be managed");
        // non-existent path is also not managed
        assert!(!config_is_managed(&dir.path().join("missing.kdl")));
    }
}
