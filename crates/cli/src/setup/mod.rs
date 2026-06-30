//! `zj-radar setup [codex|zellij]` — idempotent, conflict-aware local wiring.
//! Claude is handled by the marketplace plugin; Zellij setup installs the wasm
//! at a stable path and manages the `radar` plugin alias in `config.kdl`.

mod analyze;
mod check;
mod codex;
mod download;
mod edit;
mod zellij;
pub(crate) use analyze::*;
pub(crate) use check::*;
pub(crate) use codex::*;
pub(crate) use download::*;
pub(crate) use edit::*;
pub(crate) use zellij::*;

use std::path::{Path, PathBuf};

/// Our legacy Codex notify invocation — also the idempotency/uninstall marker.
pub(crate) const CODEX_NOTIFY_MARKER: [&str; 3] = ["zj-radar", "notify", "codex"];
// Also used by `run`'s producer detection so the two agree on what marks a wired
// Codex producer (shared single source of truth).
pub(crate) const CODEX_HOOK_MARKER: &str = "ZJ_RADAR_CODEX_HOOK=v1";
pub(crate) const CODEX_HOOK_COMMAND: &str = "ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex";
pub(crate) const CODEX_HOOK_COMMAND_WINDOWS: &str =
    "cmd /C \"set ZJ_RADAR_CODEX_HOOK=v1&& zj-radar notify codex\"";
pub(crate) const CODEX_HOOK_EVENTS: [&str; 7] = [
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "Stop",
];
pub(crate) const ZELLIJ_ALIAS_BEGIN: &str = "// zj-radar: managed plugin alias begin";
pub(crate) const ZELLIJ_ALIAS_END: &str = "// zj-radar: managed plugin alias end";

pub struct SetupOptions<'a> {
    pub targets: &'a [String],
    pub wasm: Option<&'a Path>,
    /// Fetch the wasm matching this CLI's version instead of passing `--wasm`.
    pub download: bool,
    pub uninstall: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub check: bool,
    pub legacy_notify: bool,
    pub force: bool,
    /// Non-interactive consent to inject the rail into the target layout.
    pub inject: bool,
    /// Layout name to inject into (`<config_dir>/layouts/<name>.kdl`).
    /// `None` means `default`.
    pub layout: Option<&'a str>,
    /// Open the plugin in a focused floating pane so Zellij can prompt for
    /// permissions (one-time grant). Exits after launching; skips wasm/alias/inject.
    pub grant: bool,
}

/// Where the wasm artifact comes from. A total type so "both --wasm and
/// --download" is a refusal at one place, not a runtime check inside the
/// orchestrator.
pub(crate) enum WasmSource {
    None,
    Path(PathBuf),
    Download,
}

pub(crate) fn wasm_source(wasm: Option<&Path>, download: bool) -> Result<WasmSource, String> {
    match (wasm, download) {
        (Some(_), true) => Err("pass either --wasm <path> or --download, not both".to_string()),
        (Some(p), false) => Ok(WasmSource::Path(p.to_path_buf())),
        (None, true)     => Ok(WasmSource::Download),
        (None, false)    => Ok(WasmSource::None),
    }
}

pub(crate) struct ZellijSetupOpts<'a> {
    wasm_source: WasmSource,
    force:       bool,
    inject:      bool,
    layout:      Option<&'a str>,
    dry_run:     bool,
    yes:         bool,
}

pub(crate) struct CodexSetupOpts {
    legacy_notify: bool,
    force:         bool,
    dry_run:       bool,
    yes:           bool,
}

/// The single operation a `setup` invocation performs. Resolving this once makes
/// the precedence (grant > check > uninstall > install) explicit instead of
/// implicit in the order of `if` blocks.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Grant,
    Check,
    Uninstall,
    Install,
}

pub(crate) fn mode_from_flags(grant: bool, check: bool, uninstall: bool) -> Mode {
    if grant {
        Mode::Grant
    } else if check {
        Mode::Check
    } else if uninstall {
        Mode::Uninstall
    } else {
        Mode::Install
    }
}

pub(crate) fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
        .unwrap_or(false)
}

/// Entry point for `zj-radar setup`.
pub fn run(options: SetupOptions<'_>) {
    let mode = mode_from_flags(options.grant, options.check, options.uninstall);

    if mode == Mode::Grant {
        run_grant(&zellij_config_dir());
        return;
    }

    let want_codex = (options.targets.is_empty() && options.wasm.is_none() && !options.download)
        || options.targets.iter().any(|a| a == "codex");
    let want_zellij = options.targets.iter().any(|a| a == "zellij")
        || options.wasm.is_some()
        || options.download;
    for a in options
        .targets
        .iter()
        .filter(|a| !matches!(a.as_str(), "codex" | "zellij"))
    {
        eprintln!("zj-radar: setup does not support '{a}' (supported: codex, zellij). Skipping.");
    }

    if mode == Mode::Check {
        if want_zellij {
            check_zellij();
        }
        if want_codex {
            check_codex(options.legacy_notify);
        }
        return;
    }

    let uninstall = mode == Mode::Uninstall;
    if want_zellij {
        let wasm_source = if uninstall {
            WasmSource::None
        } else {
            match wasm_source(options.wasm, options.download) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("zellij: refused — {e}");
                    return;
                }
            }
        };
        setup_zellij(
            uninstall,
            ZellijSetupOpts {
                wasm_source,
                force:   options.force,
                inject:  options.inject,
                layout:  options.layout,
                dry_run: options.dry_run,
                yes:     options.yes,
            },
        );
    }
    if want_codex {
        setup_codex(
            uninstall,
            CodexSetupOpts {
                legacy_notify: options.legacy_notify,
                force:         options.force,
                dry_run:       options.dry_run,
                yes:           options.yes,
            },
        );
    }
}

pub(crate) fn confirm(prompt: &str) -> bool {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The shared "commit an edit" tail for every `setup_*` step: prompt (unless
/// `--yes`), run any `pre_write` side effects (e.g. copying the wasm), then write
/// `new` to `path` atomically — emitting the standard `skipped`/`failed`
/// diagnostics under `label`. Returns whether the file was written, so the caller
/// can print its success epilogue. Callers keep `--dry-run` handling and prompt
/// wording, which differ per target. A `pre_write` error is reported as
/// `{label}: {e}`, so its message should read as a sentence without the prefix.
pub(crate) fn confirm_and_write(
    label: &str,
    path: &Path,
    new: &str,
    yes: bool,
    prompt: &str,
    pre_write: impl FnOnce() -> Result<(), String>,
) -> bool {
    if !yes && !confirm(prompt) {
        println!("{label}: skipped (declined)");
        return false;
    }
    if let Err(e) = pre_write() {
        eprintln!("{label}: {e}");
        return false;
    }
    if let Err(e) = write_atomic(path, new) {
        eprintln!("{label}: write failed — {e}");
        return false;
    }
    true
}

/// Back up the existing file, then write atomically (temp file + rename via the
/// shared `fsutil::atomic_write`). The `.bak` is specific to `setup` editing the
/// user's own files; `run` writes its owned dir without one.
pub(crate) fn write_atomic(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if path.exists() {
        let _ = std::fs::copy(path, path_with_suffix(path, ".zj-radar.bak"));
    }
    crate::fsutil::atomic_write(path, contents.as_bytes())
}

pub(crate) fn path_with_suffix(path: &std::path::Path, suffix: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| format!("{}{}", name.to_string_lossy(), suffix))
        .unwrap_or_else(|| format!("config{suffix}"));
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_source_rejects_both_path_and_download() {
        let p = std::path::Path::new("/x.wasm");
        assert!(matches!(wasm_source(Some(p), false), Ok(WasmSource::Path(_))));
        assert!(matches!(wasm_source(None, true), Ok(WasmSource::Download)));
        assert!(matches!(wasm_source(None, false), Ok(WasmSource::None)));
        assert!(wasm_source(Some(p), true).is_err(), "both --wasm and --download must refuse");
    }

    #[test]
    fn mode_precedence_grant_beats_check_beats_uninstall() {
        assert!(matches!(mode_from_flags(true, true, true), Mode::Grant));
        assert!(matches!(mode_from_flags(false, true, true), Mode::Check));
        assert!(matches!(mode_from_flags(false, false, true), Mode::Uninstall));
        assert!(matches!(mode_from_flags(false, false, false), Mode::Install));
    }
}
