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
/// 3. `--dry-run` → `Snippet` (same safe default — a write-nothing flag must
///    never block on stdin; combine with `--inject` to preview the inject).
/// 4. Not a tty → `Snippet` (no way to ask).
/// 5. Otherwise → `Prompt`  (interactive).
pub(crate) fn inject_mode(inject_flag: bool, yes: bool, dry_run: bool, is_tty: bool) -> InjectMode {
    if inject_flag {
        return InjectMode::Inject;
    }
    if yes || dry_run || !is_tty {
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
            crate::exit::fail_report(
                "zj-radar",
                format!(
                    "zellij plugin exited with {status}; \
                     try running: zellij {}",
                    crate::run::shell_join(&args)
                ),
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            crate::exit::fail_report(
                "zj-radar",
                format!(
                    "zellij not found on PATH — install Zellij {SUPPORTED_ZELLIJ_MINOR}.{MIN_SUPPORTED_ZELLIJ_PATCH}+ first \
                     (https://zellij.dev/documentation/installation)"
                ),
            );
        }
        Err(e) => {
            crate::exit::fail_report(
                "zj-radar",
                format!(
                    "failed to launch zellij for grant — {e}; \
                     try running: zellij {}",
                    crate::run::shell_join(&args)
                ),
            );
        }
    }
}

/// `zellij --version` stdout (trimmed), or `None` when the binary is absent
/// or unrunnable — the raw read behind `ZellijEnv::zellij_version`.
pub(crate) fn zellij_version_output() -> Option<String> {
    let out = std::process::Command::new("zellij").arg("--version").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(crate) fn zellij_config_dir() -> Option<PathBuf> {
    resolve_config_dir(
        std::env::var_os("ZELLIJ_CONFIG_DIR"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// The resolved config dir, or `None` with the refusal already reported —
/// callers just early-return. With every env source unset, the old fallback
/// resolved to the *relative* path `.config/zellij` in the process CWD, so a
/// `setup zellij --yes` from cron/a container silently grew a config tree in
/// whatever directory it ran from (mirrors `codex_home_from`'s refusal).
pub(crate) fn zellij_config_dir_or_report() -> Option<PathBuf> {
    let dir = zellij_config_dir();
    if dir.is_none() {
        crate::exit::fail_report(
            "zellij",
            "skipped — set $HOME (or $ZELLIJ_CONFIG_DIR / $XDG_CONFIG_HOME) \
             so the Zellij config dir can be resolved",
        );
    }
    dir
}

/// Mirror Zellij's own config-dir resolution so `setup` writes where Zellij reads:
/// `ZELLIJ_CONFIG_DIR` wins, then `$XDG_CONFIG_HOME/zellij`, then `~/.config/zellij`.
/// Skipping `XDG_CONFIG_HOME` would install the alias/wasm into `~/.config` while a
/// Linux XDG user's Zellij reads elsewhere — the rail never appears and `--check`
/// reports everything missing. `None` when every source is unset or empty — writers
/// must refuse rather than invent a path. Pure (env read by the caller) so the
/// precedence is unit-testable without mutating process-global environment.
fn resolve_config_dir(
    zellij_config_dir: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(dir) = zellij_config_dir.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = xdg_config_home.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("zellij"));
    }
    home.filter(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join(".config").join("zellij"))
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

/// Refusal for a bare `setup zellij` with no wasm source: name the install
/// routes AND the supported wasm-less invocations, so the message is a menu,
/// not a dead end.
const NO_WASM_REFUSAL: &str =
    "refused — pass --wasm <path-to-zj_radar.wasm> or --download to install; \
     or use --inject (add the rail to a layout only) or --check (inspect the current state)";

/// Which path a `setup zellij` invocation takes. [`setup_path`] decides purely
/// from the facts the orchestrator's old early-return ladder read;
/// `setup_zellij` matches on it and keeps all the IO. Ordering is part of the
/// contract: a managed config wins over everything (a symlinked config.kdl is
/// never rewritten), then the wasm-less modes resolve, and only then does the
/// full flow run.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SetupPath {
    /// config.kdl is a symlink (Nix / home-manager): install prints the layout
    /// snippet for guidance; uninstall skips the config rewrite (the alias
    /// lives in the user's Nix config, so removal is their Nix concern) but
    /// still strips the injected rail from the (separate) layout file.
    Managed,
    /// Install with no wasm source but `--inject`/`--yes` consent: skip the
    /// wasm/alias step and run the layout step only. This makes `setup zellij
    /// --inject` and `setup zellij --yes` usable and testable without a wasm
    /// artifact.
    LayoutOnlyInstall,
    /// `--uninstall` with no wasm source and no config.kdl on disk: only the
    /// layout could hold anything of ours.
    LayoutOnlyUninstall,
    /// Bare install with no wasm source and no wasm-less consent flag: refuse
    /// with [`NO_WASM_REFUSAL`].
    RefuseNoWasm,
    /// The full wasm + alias (+ layout) install/uninstall flow.
    Full,
}

pub(crate) fn setup_path(
    uninstall: bool,
    config_managed: bool,
    has_wasm_source: bool,
    inject: bool,
    yes: bool,
    config_exists: bool,
) -> SetupPath {
    if config_managed {
        return SetupPath::Managed;
    }
    if !uninstall && !has_wasm_source {
        return if inject || yes { SetupPath::LayoutOnlyInstall } else { SetupPath::RefuseNoWasm };
    }
    // An uninstall with a config.kdl on disk still runs the full flow — the
    // alias may need stripping.
    if uninstall && !has_wasm_source && !config_exists {
        return SetupPath::LayoutOnlyUninstall;
    }
    SetupPath::Full
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
    let Some(config_dir) = zellij_config_dir_or_report() else { return };

    // One reader, shared with `check` (`read_zellij_env`): current state into
    // Facts. The env's config text is reused below for the `edit_zellij`
    // splice; the resolved paths are what every write below aims at.
    let (env, paths) = read_zellij_env(&config_dir, layout_name);
    let ZellijPaths { config_path, wasm_dest, layout_path } = paths;
    let location = zellij_plugin_location(&wasm_dest);
    let facts = analyze_zellij(&env);

    let path = setup_path(
        uninstall,
        facts.config_managed,
        wasm.is_some() || download,
        inject_flag,
        yes,
        config_path.exists(),
    );
    match path {
        SetupPath::Managed => {
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
        SetupPath::LayoutOnlyInstall => {
            run_layout_inject(&layout_path, inject_flag, yes, dry_run);
            return;
        }
        SetupPath::LayoutOnlyUninstall => {
            run_layout_uninstall(&layout_path, dry_run);
            return;
        }
        SetupPath::RefuseNoWasm => {
            crate::exit::fail_report("zellij", NO_WASM_REFUSAL);
            return;
        }
        SetupPath::Full => {}
    }

    // Resolve the wasm source: an explicit --wasm path, or --download (fetch the
    // wasm matching this CLI's version). `downloaded` outlives the borrow in `src`.
    // Under --dry-run the fetch is skipped ("write nothing" must also mean "works
    // offline"): the config splice below needs only the *destination* path, and
    // the dry-run arm never copies — `src` stays None, announced here instead.
    let downloaded: PathBuf;
    let src: Option<&Path> = if uninstall || (download && dry_run) {
        None
    } else if download {
        match download_wasm(&wasm_download_version()) {
            Ok(path) => {
                downloaded = path;
                Some(downloaded.as_path())
            }
            Err(e) => {
                crate::exit::fail_report("zellij", format!("refused — {e}"));
                return;
            }
        }
    } else {
        wasm
    };

    if !uninstall {
        if download && dry_run {
            println!(
                "zellij: would download zj_radar.wasm v{} -> {} (dry-run)",
                wasm_download_version(),
                wasm_dest.display()
            );
        } else {
            // `SetupPath::Full` guarantees a wasm source on install (see
            // `setup_path`), so the None arm is defensive only.
            let Some(src) = src else {
                crate::exit::fail_report("zellij", NO_WASM_REFUSAL);
                return;
            };
            if !src.is_file() {
                crate::exit::fail_report("zellij", format!("refused — wasm not found at {}", src.display()));
                return;
            }
        }
    }

    let existing = env.config_text.unwrap_or_default();
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
            // alias already up to date — still offer injection and the grant.
            run_layout_inject(&layout_path, inject_flag, yes, dry_run);
            if !run_preseed(&wasm_dest, facts.granted, yes, dry_run) {
                print_grant_hint_if_needed(&facts);
            }
            print_producer_hint_if_needed(&facts);
        }
        Outcome::Conflict => {
            crate::exit::fail_report(
                "zellij",
                format!(
                    "{} already has an unmanaged `radar` plugin alias. Refusing to overwrite it.\n\
                     Re-run with --force to replace it, or wire zj-radar manually.",
                    config_path.display()
                ),
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
                    run_preseed(&wasm_dest, facts.granted, yes, dry_run);
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
                if !run_preseed(&wasm_dest, facts.granted, yes, dry_run) {
                    print_grant_hint_if_needed(&facts);
                }
                print_producer_hint_if_needed(&facts);
            }
        }
    }
}

/// Pre-authorize the sidebar's permissions in Zellij's `permissions.kdl` at
/// the tail of a `setup zellij` install. Returns whether the grant is in
/// place afterwards (pre-existing or just written) — `false` routes the
/// caller to the manual first-launch hint. Zellij reads the file fresh on
/// every plugin load, so the sidebar's own `request_permission` auto-resolves
/// against this entry and the user never meets Zellij's native prompt, which
/// is illegible at rail width (zellij#4749). Best-effort by design: every
/// refusal degrades to the hint, never to a failed install.
fn run_preseed(wasm_dest: &Path, granted: Option<bool>, yes: bool, dry_run: bool) -> bool {
    use super::preseed::{merge_grant, Preseed};
    if granted == Some(true) {
        return true;
    }
    let Some(perms_path) = crate::run::zellij_permissions_path() else {
        return false; // no resolvable cache dir — the hint still covers first launch
    };
    let existing = std::fs::read_to_string(&perms_path).ok();
    let merged = match merge_grant(existing.as_deref(), &wasm_dest.display().to_string()) {
        Ok(Preseed::AlreadyGranted) => return true,
        Ok(Preseed::Merged(text)) => text,
        Err(e) => {
            eprintln!("zellij: not pre-authorizing permissions — {e}");
            return false;
        }
    };
    if dry_run {
        println!(
            "zellij: would pre-authorize the sidebar's permissions in {} (dry-run)",
            perms_path.display()
        );
        return false;
    }
    // Its own consent line, separate from the config/layout prompts: this one
    // writes a *grant* into a file Zellij owns, so name every permission.
    let prompt = format!(
        "Pre-authorize the sidebar's Zellij permissions ({}) in {}?",
        crate::run::REQUIRED_PLUGIN_PERMISSIONS.join(", "),
        perms_path.display()
    );
    if !yes && !super::confirm(&prompt) {
        println!("zellij: skipped permission pre-authorization");
        return false;
    }
    if let Some(parent) = perms_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("zellij: not pre-authorizing permissions — create cache dir failed ({e})");
            return false;
        }
    }
    if let Err(e) = super::write_atomic(&perms_path, &merged) {
        eprintln!("zellij: not pre-authorizing permissions — write failed ({e})");
        return false;
    }
    println!(
        "zellij: pre-authorized the sidebar ({}) — the rail is live on next launch; \
         already-running sessions pick it up on a new tab or restart",
        perms_path.display()
    );
    true
}

/// Emit the first-launch grant instructions at the tail of a `setup zellij`
/// install, unless permissions.kdl already grants this wasm (`facts.granted`,
/// read the same way `check_zellij` reads it) — a long-granted install
/// re-running setup needs no onboarding walkthrough.
fn print_grant_hint_if_needed(facts: &ZellijFacts) {
    if facts.granted == Some(true) {
        return;
    }
    // Only reached when the pre-seed didn't land (declined, unresolvable cache
    // dir, or a malformed permissions.kdl we refused to edit). The rail can't
    // show Zellij's grant prompt legibly (it's a small borderless pane —
    // Zellij #4749), so say plainly what the user will see: a blank rail.
    println!(
        "zellij: permissions not pre-authorized — on first launch the rail will look \
         BLANK while Zellij's prompt (unreadable at rail width) waits. Focus the rail \
         and press y to allow access, or run `zj-radar setup zellij --grant` from \
         inside Zellij for a legible floating prompt. Zellij asks once, then remembers."
    );
}

/// Emit a producer hint at the tail of `setup zellij` when no producer is wired,
/// per `facts.producer_wired` (derived from Codex hooks + the Claude plugin
/// manifest, same as `run`'s detection — see `analyze_zellij`).
fn print_producer_hint_if_needed(facts: &ZellijFacts) {
    if !facts.producer_wired() {
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
/// prints the tailored snippet. A missing layout file takes the *create* path:
/// with consent, the full known-good layout is written fresh — a stock Zellij
/// ships no layout file at all, so "paste this fragment" alone is a dead end
/// (there is nothing to paste it into).
fn run_layout_inject(layout_path: &Path, inject_flag: bool, yes: bool, dry_run: bool) {
    use std::io::IsTerminal;
    let is_tty = std::io::stdin().is_terminal();
    let mode = inject_mode(inject_flag, yes, dry_run, is_tty);

    // Same Nix / home-manager guard config.kdl gets (`SetupPath::Managed`): the
    // inject below writes via atomic rename, which would silently replace a
    // symlink with a regular file — the next `home-manager switch` reverts it
    // and the rail "mysteriously vanishes". Snippet instead, never a write.
    if config_is_managed(layout_path) {
        eprintln!(
            "zellij: layout at {} is a symlink (managed by Nix / home-manager) — \
             zj-radar will not overwrite a managed layout; add the rail via your \
             Nix config instead. See docs/install.md for the home-manager snippet.",
            layout_path.display()
        );
        print_snippet_for(layout_path);
        return;
    }

    let text = match std::fs::read_to_string(layout_path) {
        Ok(t) => t,
        // Only a genuinely absent file takes the create path below. Any other
        // read error (EACCES, non-UTF8, …) means a layout EXISTS but can't be
        // read — consenting to "create it?" would replace the user's file
        // under a prompt that lied, so refuse and leave the snippet.
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            crate::exit::fail_report(
                "zellij",
                format!("could not read layout {} — {e}; leaving it untouched", layout_path.display()),
            );
            eprintln!("        Add the rail manually using the snippet below.");
            print_paste_snippet(&crate::layout::analyze(""));
            return;
        }
        Err(_) => {
            match mode {
                InjectMode::Inject => create_full_layout(layout_path, dry_run),
                InjectMode::Prompt => {
                    let prompt = format!(
                        "No layout at {} — create it with the rail layout?",
                        layout_path.display()
                    );
                    if confirm(&prompt) {
                        create_full_layout(layout_path, dry_run);
                    } else {
                        print_missing_layout_fallback(layout_path);
                    }
                }
                // --yes / non-tty: the safe non-mutating default, as ever.
                InjectMode::Snippet => print_missing_layout_fallback(layout_path),
            }
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

/// Write the full known-good layout (byte-pinned to `run_assets/radar.kdl`)
/// to a path that has no layout file yet. The file is new, so `write_atomic`'s
/// backup step is naturally skipped and there is nothing to corrupt.
fn create_full_layout(layout_path: &Path, dry_run: bool) {
    let layout = crate::layout::full_layout();
    if dry_run {
        println!(
            "zellij: would create {} with the rail layout (dry-run)\n--- layout (dry-run) ---\n{layout}",
            layout_path.display()
        );
        return;
    }
    match write_atomic(layout_path, &layout) {
        Ok(()) => println!("zellij: created {} with the rail layout", layout_path.display()),
        Err(e) => crate::exit::fail_report("zellij", format!("layout create failed — {e}")),
    }
}

/// The declined / non-consenting fallback when no layout file exists: print
/// the fragment snippet, but say what it needs to live in and how to get the
/// file created — this branch must not read as a working end state (the rail
/// will not appear without a layout).
fn print_missing_layout_fallback(layout_path: &Path) {
    let facts = crate::layout::analyze("");
    let snippet = crate::layout::tailored_snippet(&facts);
    println!(
        "zellij: no layout at {} — the rail won't appear until one exists.\n\
         Re-run with --inject (or answer y) to create it, or wrap the snippet\n\
         below in a `layout {{ … }}` block yourself:\n\n{snippet}",
        layout_path.display()
    );
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
                print_swap_advisory_if_needed(facts);
                return;
            }
            // Back up then atomically write (shared setup helper).
            match write_atomic(layout_path, &new_text) {
                Ok(()) => {
                    println!(
                        "zellij: rail injected into {} (backup: {}.zj-radar.bak)",
                        layout_path.display(),
                        layout_path.display()
                    );
                    print_swap_advisory_if_needed(facts);
                }
                Err(e) => crate::exit::fail_report("zellij", format!("write failed — {e}")),
            }
        }
        Err(crate::layout::Refusal::Unparseable(msg)) => {
            crate::exit::fail_report("zellij", format!("layout could not be parsed — {msg}"));
            eprintln!("        Add the rail manually using the snippet below.");
            let snippet = crate::layout::tailored_snippet(facts);
            println!("\n{snippet}");
        }
        Err(crate::layout::Refusal::Unrecognized(msg)) => {
            crate::exit::fail_report("zellij", format!("layout shape not recognized — {msg}"));
            eprintln!("        Add the rail manually using the snippet below.");
            let snippet = crate::layout::tailored_snippet(facts);
            println!("\n{snippet}");
        }
    }
}

/// After injecting into a layout that declares its own `swap_tiled_layout`
/// blocks, tell the user what inject deliberately did NOT do: their swaps were
/// left untouched (we never rewrite bodies we didn't author), so Alt+[ / Alt+]
/// still cycles to rail-less layouts until they route each swap entry through
/// the injected `ui` template. Silence here would read as "all wired" right up
/// until the first swap pops the rail.
fn print_swap_advisory_if_needed(facts: &crate::layout::LayoutFacts) {
    if facts.has_swaps {
        println!(
            "zellij: note — your layout has its own swap_tiled_layout blocks, which were \
             left untouched. Alt+[ / Alt+] will swap to layouts without the rail until \
             each swap entry is routed through the injected `ui` template \
             (`ui max_panes=N {{ ... }}`) — see docs/troubleshooting.md."
        );
    }
}

/// Handle `--uninstall` for the layout: strip the injected rail if present.
/// A layout with no injection markers can still be ours: the no-layout install
/// path writes `full_layout()` whole (see `create_full_layout`). Since the
/// config step has just stripped the `radar` alias, leaving that layout behind
/// strands the next plain Zellij launch on a dead alias — so a byte-identical
/// generated layout is deleted (it is entirely ours), and any OTHER
/// marker-less layout that still references the alias gets an advisory, never
/// a delete (we don't remove files we can't prove we authored).
fn run_layout_uninstall(layout_path: &Path, dry_run: bool) {
    // Symlink = Nix / home-manager territory, same as the inject guard above:
    // both the strip-rewrite and the whole-file delete below would replace or
    // remove a file the user's Nix config owns.
    if config_is_managed(layout_path) {
        eprintln!(
            "zellij: layout at {} is a symlink (managed by Nix / home-manager) — \
             zj-radar will not modify a managed layout; remove the rail via your \
             Nix config instead.",
            layout_path.display()
        );
        return;
    }
    let text = match std::fs::read_to_string(layout_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return, // no layout — nothing to uninstall
        // An unreadable layout is not an absent one: report it rather than
        // silently claiming there was nothing of ours to remove.
        Err(e) => {
            crate::exit::fail_report(
                "zellij",
                format!("could not read layout {} — {e}; leaving it untouched", layout_path.display()),
            );
            return;
        }
    };
    match crate::layout::uninstall(&text) {
        None if text == crate::layout::full_layout() => {
            if dry_run {
                println!(
                    "zellij: would delete {} (dry-run) — created whole by setup, and it \
                     references the removed `radar` alias",
                    layout_path.display()
                );
                return;
            }
            match std::fs::remove_file(layout_path) {
                Ok(()) => println!(
                    "zellij: deleted {} (created whole by setup; it referenced the \
                     removed `radar` alias)",
                    layout_path.display()
                ),
                Err(e) => crate::exit::fail_report("zellij", format!("layout delete failed — {e}")),
            }
        }
        None if text.contains("plugin location=\"radar\"") => {
            println!(
                "zellij: note — {} still references the removed `radar` alias — delete it \
                 or restore the alias, or the next Zellij launch will fail to resolve it",
                layout_path.display()
            );
        }
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
                Err(e) => crate::exit::fail_report("zellij", format!("write failed — {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    // ── config-dir resolution (matches Zellij's own precedence) ──────────────
    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn resolve_config_dir_prefers_zellij_config_dir() {
        let got = resolve_config_dir(os("/explicit"), os("/xdg"), os("/home"));
        assert_eq!(got, Some(PathBuf::from("/explicit")));
    }

    #[test]
    fn resolve_config_dir_falls_back_to_xdg_then_home() {
        // No ZELLIJ_CONFIG_DIR → XDG_CONFIG_HOME/zellij (the bug: this used to be
        // skipped, so XDG users got a silently-ineffective setup).
        assert_eq!(
            resolve_config_dir(None, os("/xdg"), os("/home")),
            Some(PathBuf::from("/xdg/zellij")),
        );
        // No ZELLIJ_CONFIG_DIR, no XDG → ~/.config/zellij.
        assert_eq!(
            resolve_config_dir(None, None, os("/home")),
            Some(PathBuf::from("/home/.config/zellij")),
        );
        // Empty env values are ignored, not treated as "" paths.
        assert_eq!(
            resolve_config_dir(os(""), os(""), os("/home")),
            Some(PathBuf::from("/home/.config/zellij")),
        );
    }

    #[test]
    fn resolve_config_dir_refuses_when_no_source_is_set() {
        // With every env source unset (or empty), the old `unwrap_or_default`
        // fallback produced the RELATIVE path `.config/zellij` — a setup run
        // from cron/a container would grow a config tree in its CWD. Writers
        // must refuse instead.
        assert_eq!(resolve_config_dir(None, None, None), None);
        assert_eq!(resolve_config_dir(os(""), os(""), os("")), None);
    }

    // ── inject_mode decision tests ───────────────────────────────────────────

    #[test]
    fn inject_flag_forces_inject() {
        assert_eq!(inject_mode(true, false, false, false), InjectMode::Inject);
        assert_eq!(inject_mode(true, false, false, true), InjectMode::Inject);
        assert_eq!(inject_mode(true, true, false, false), InjectMode::Inject);
        assert_eq!(inject_mode(true, true, false, true), InjectMode::Inject);
    }

    #[test]
    fn yes_takes_safe_default_snippet() {
        // --yes without --inject → Snippet regardless of tty
        assert_eq!(inject_mode(false, true, false, true),  InjectMode::Snippet);
        assert_eq!(inject_mode(false, true, false, false), InjectMode::Snippet);
    }

    #[test]
    fn dry_run_never_prompts() {
        // A write-nothing flag must never block on stdin — even on a tty, a
        // bare `--download --dry-run` takes the snippet preview, not a prompt.
        assert_eq!(inject_mode(false, false, true, true), InjectMode::Snippet);
        assert_eq!(inject_mode(false, false, true, false), InjectMode::Snippet);
        // With explicit --inject it previews the inject itself instead.
        assert_eq!(inject_mode(true, false, true, true), InjectMode::Inject);
    }

    #[test]
    fn non_tty_takes_safe_default_snippet() {
        // non-tty without --inject or --yes → Snippet
        assert_eq!(inject_mode(false, false, false, false), InjectMode::Snippet);
    }

    #[test]
    fn prompt_when_interactive() {
        // interactive tty, no --inject, no --yes, no --dry-run → Prompt
        assert_eq!(inject_mode(false, false, false, true), InjectMode::Prompt);
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

    // ── setup_path decision table ────────────────────────────────────────────

    #[test]
    fn setup_path_covers_the_whole_ladder() {
        use SetupPath::*;
        // (uninstall, config_managed, has_wasm_source, inject, yes, config_exists)
        let cases = &[
            // A managed (symlinked) config wins over everything else.
            ((false, true, true,  true,  true,  true ), Managed),
            ((true,  true, false, false, false, true ), Managed),
            // Install, no wasm source: --inject or --yes → layout-only …
            ((false, false, false, true,  false, true ), LayoutOnlyInstall),
            ((false, false, false, false, true,  false), LayoutOnlyInstall),
            // … and a bare invocation refuses with guidance.
            ((false, false, false, false, false, true ), RefuseNoWasm),
            // Uninstall with no config.kdl on disk: only the layout can hold ours.
            ((true,  false, false, false, false, false), LayoutOnlyUninstall),
            // Uninstall with a config.kdl: full flow (the alias may need stripping).
            ((true,  false, false, false, false, true ), Full),
            // Install with a wasm source: full flow, flags notwithstanding.
            ((false, false, true,  false, false, false), Full),
            ((false, false, true,  true,  true,  true ), Full),
        ];
        for ((uninstall, managed, has_wasm, inject, yes, config_exists), want) in cases {
            assert_eq!(
                &setup_path(*uninstall, *managed, *has_wasm, *inject, *yes, *config_exists),
                want,
                "uninstall={uninstall} managed={managed} has_wasm={has_wasm} \
                 inject={inject} yes={yes} config_exists={config_exists}"
            );
        }
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
