//! `zj-radar setup [claude|codex|opencode|zellij]` — idempotent,
//! conflict-aware local wiring. Claude is wired through Claude Code's own
//! plugin marketplace (we drive the `claude plugin` CLI, never its files);
//! Codex gets hooks.json entries; Opencode gets a vendored JS bridge plugin
//! dropped into its auto-loaded global plugins dir (no `opencode.json` edit);
//! Zellij setup installs the wasm at a stable path and manages the `radar`
//! alias in `config.kdl`.

mod analyze;
mod check;
mod claude;
mod codex;
mod detect;
mod download;
mod edit;
mod opencode;
mod preseed;
mod zellij;
pub(crate) use analyze::*;
pub(crate) use check::*;
pub(crate) use claude::*;
pub(crate) use codex::*;
pub(crate) use download::*;
pub(crate) use edit::*;
pub(crate) use opencode::*;
pub(crate) use zellij::*;

use std::path::{Path, PathBuf};

/// Our legacy Codex notify invocation — also the idempotency/uninstall marker.
pub(crate) const CODEX_NOTIFY_MARKER: [&str; 3] = ["zj-radar", "notify", "codex"];
// Also used by `run`'s producer detection so the two agree on what marks a wired
// Codex producer (shared single source of truth).
pub(crate) const CODEX_HOOK_MARKER: &str = "ZJ_RADAR_CODEX_HOOK=v1";
pub(crate) const CODEX_HOOK_COMMAND: &str = "ZJ_RADAR_CODEX_HOOK=v1 zj-radar notify codex";
// No space before the `&&`: cmd.exe folds everything up to the separator into
// the env var's VALUE, so `v1 &&` would set the marker to "v1 " and break the
// marker round-trip that idempotency/uninstall detection depends on.
pub(crate) const CODEX_HOOK_COMMAND_WINDOWS: &str =
    "cmd /C \"set ZJ_RADAR_CODEX_HOOK=v1&& zj-radar notify codex\"";
// Kill ceiling for the generated hook entries — derived from the send cap,
// never hand-copied: the CLI's graceful bounded no-op lands at ~cap (its
// in-subtree watchdog kills the pipe client at the deadline) with a cap + 1 s
// parent-reaper backstop, so a ceiling equal to the cap means the hook runner
// always kills the hook first and the clean-exit contract is dead code. cap +
// 5 leaves the backstop plus generous spawn/derivation slack, matching the
// Claude plugin's edge hooks (welded there by the headroom guard in
// crates/plugin's hooks_manifest_tests).
pub(crate) const CODEX_HOOK_TIMEOUT_SECS: u64 = crate::pipe::DEFAULT_PIPE_TIMEOUT_SECS + 5;
// `PostToolUse` looks like a pure duplicate of `PreToolUse` (both derive the
// same activity string from the same tool payload), but it is load-bearing:
// after a `PermissionRequest` mid-turn flips the pane to Pending, the tool's
// Post broadcast is the Pending→Running recovery edge — without it an
// answered prompt stays flagged "needs you" until the next tool or Stop. The
// duplicate case is instead de-duplicated on the RECEIVING side: the plugin's
// `status_pipe` treats an identical re-broadcast as a strict no-op (see
// `CONTEXT.md` → Render gate), so keeping Post costs only the pipe delivery,
// not a render+persist per rail instance.
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
// The one-time trust step Codex requires before it runs installed hooks. One
// copy (the CODEX_HOOK_MARKER precedent) shared by setup's install epilogue
// and the doctor's note item, so the advice can't drift between them. No
// trailing punctuation — callers supply their own.
pub(crate) const CODEX_HOOK_TRUST_ADVICE: &str =
    "run `/hooks` in Codex to review and trust the zj-radar command hook";

/// The idempotency/ownership marker stamped into the vendored opencode bridge
/// plugin's header. One shared source of truth (the CODEX_HOOK_MARKER
/// precedent) behind `setup opencode`'s install/uninstall gating, the doctor,
/// and `run`'s producer detection, so the three can't drift on what counts as
/// "ours". The JS plugin carries it as a leading comment: `// ZJ_RADAR_OPENCODE_PLUGIN=v1`.
pub(crate) const OPENCODE_PLUGIN_MARKER: &str = "ZJ_RADAR_OPENCODE_PLUGIN=v1";
/// The filename opencode auto-loads from its global plugins dir — the single
/// install/uninstall target (no `opencode.json` edit, clean uninstall = delete
/// one file). Inside the cargo package via `include_str!` so `cargo publish`
/// works (a repo-root path would break crates.io installs).
pub(crate) const OPENCODE_PLUGIN_FILE_NAME: &str = "zj-radar.js";
/// The vendored bridge plugin, embedded so `setup opencode` writes a known-good
/// copy and `cargo publish` packages it. A `flake.nix` filter entry covers this
/// non-Rust `include_str!` input for the hermetic build.
pub(crate) const OPENCODE_PLUGIN_JS: &str = include_str!("opencode_plugin.js");

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
    is_tty:      bool,
}

pub(crate) struct CodexSetupOpts {
    legacy_notify: bool,
    force:         bool,
    dry_run:       bool,
    yes:           bool,
    is_tty:        bool,
}

pub(crate) struct OpencodeSetupOpts {
    pub force:   bool,
    pub dry_run: bool,
    pub yes:     bool,
    pub is_tty:  bool,
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

/// Clap already hard-errors on `--check --uninstall` (and `--grant` with
/// either), so the check-beats-uninstall rung is defensive, not a CLI surface.
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
        // Same cross-target hygiene as --inject/--layout below: --grant opens
        // the ZELLIJ permission float, so `setup codex --grant` silently
        // granting zellij would read as "codex got wired".
        for a in options.targets.iter().filter(|a| a.as_str() != "zellij") {
            eprintln!("zj-radar: --grant applies to the zellij target only — ignoring '{a}'");
        }
        if let Some(config_dir) = zellij_config_dir_or_report() {
            run_grant(&config_dir);
        }
        return;
    }

    let bare = options.targets.is_empty() && options.wasm.is_none() && !options.download;
    let want_codex = bare || options.targets.iter().any(|a| a == "codex");
    // Bare `setup` = detected agents only: claude and opencode join codex
    // there, and each `setup_*` itself skips gracefully when the binary is
    // absent.
    let want_opencode = bare || options.targets.iter().any(|a| a == "opencode");
    // Bare `setup` = detected agents only: claude joins codex there, and
    // `setup_claude` itself skips gracefully when the binary is absent.
    let want_claude = bare || options.targets.iter().any(|a| a == "claude");
    let want_zellij = options.targets.iter().any(|a| a == "zellij")
        || options.wasm.is_some()
        || options.download;
    for a in options
        .targets
        .iter()
        .filter(|a| !matches!(a.as_str(), "claude" | "codex" | "opencode" | "zellij"))
    {
        crate::exit::fail_report("zj-radar", format!("setup does not support '{a}' (supported: claude, codex, opencode, zellij). Skipping."));
    }
    // Cross-target flag hygiene: `--wasm`/`--download` *imply* the zellij
    // target (they're zellij artifacts, see `want_zellij`), but `--inject`/
    // `--layout` do not — silently ignoring them on a codex-only invocation
    // would read as "injected". Say so instead.
    if !want_zellij {
        if options.inject {
            eprintln!("zj-radar: --inject applies to the zellij target only — add `zellij` to the targets to use it");
        }
        if options.layout.is_some() {
            eprintln!("zj-radar: --layout applies to the zellij target only — add `zellij` to the targets to use it");
        }
    }
    if want_zellij && options.legacy_notify && !want_codex {
        eprintln!("zj-radar: --legacy-notify applies to the codex target only — add `codex` to the targets to use it");
    }

    if mode == Mode::Check {
        // Bare `--check` is the doctor: inspect BOTH halves. (A bare *install*
        // defaults to codex-only because a zellij install needs a wasm source;
        // checking needs none, and a user asking "is my install healthy?"
        // wants the rail's state too, not silence about it.)
        let both = options.targets.is_empty();
        let mut missing = false;
        if want_zellij || both {
            missing |= check_zellij(options.layout);
        }
        if want_codex || both {
            missing |= check_codex(options.legacy_notify);
        }
        // Explicit `setup claude --check` always reports; the bare doctor
        // includes claude only when the binary is present (detected agents),
        // so a claude-less machine's doctor isn't failed by an agent it
        // doesn't have.
        if options.targets.iter().any(|a| a == "claude") || (both && which("claude")) {
            missing |= check_claude();
        }
        // opencode's doctor is binary-gated the same way: a machine without
        // opencode shouldn't be failed by an agent it doesn't have.
        if options.targets.iter().any(|a| a == "opencode") || (both && which("opencode")) {
            missing |= check_opencode();
        }
        if missing {
            // The items above are the diagnostic; this sets the exit code so
            // `zj-radar setup --check && zj-radar run` can gate on the doctor.
            crate::exit::fail_report("zj-radar", "check found missing items (listed above)");
        }
        return;
    }

    let uninstall = mode == Mode::Uninstall;
    // Tty-ness resolved ONCE at the invocation boundary (the `inject_mode`
    // pattern); every consent step in the targets below takes it as a
    // parameter rather than probing stdin again mid-chain.
    let is_tty = {
        use std::io::IsTerminal;
        std::io::stdin().is_terminal()
    };
    if want_zellij {
        let wasm_source = if uninstall {
            WasmSource::None
        } else {
            match wasm_source(options.wasm, options.download) {
                Ok(s) => s,
                Err(e) => {
                    crate::exit::fail_report("zellij", format!("refused — {e}"));
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
                is_tty,
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
                is_tty,
            },
        );
    }
    if want_claude {
        setup_claude(uninstall, options.dry_run, options.yes, is_tty);
    }
    if want_opencode {
        setup_opencode(
            uninstall,
            OpencodeSetupOpts {
                force:   options.force,
                dry_run: options.dry_run,
                yes:     options.yes,
                is_tty,
            },
        );
    }
}

/// The shared preamble for every `setup_*` step: turn an editor's
/// `Result<Outcome, String>` into an `Option<Outcome>`, reporting a refusal as the
/// standard `{label}: refused — {e}` line and yielding `None` so the caller bails.
/// Centralizes the one diagnostic all three orchestrators printed by hand.
pub(crate) fn edit_or_report(label: &str, edit: Result<Outcome, String>) -> Option<Outcome> {
    match edit {
        Ok(outcome) => Some(outcome),
        Err(e) => {
            crate::exit::fail_report(label, format!("refused — {e}"));
            None
        }
    }
}

/// Ask for consent. `yes` (`-y`) grants without asking; tty-ness arrives as a
/// parameter, resolved ONCE at the invocation boundary (the `inject_mode`
/// pattern) — probing `stdin().is_terminal()` in here made the answer
/// untestable and let one chain probe twice. No tty = no one to answer: take
/// the safe "no" instead of blocking on a read that never returns (the same
/// non-tty rule `inject_mode` and `run`'s foreign-session consent apply), and
/// say how to consent non-interactively.
pub(crate) fn confirm(prompt: &str, yes: bool, is_tty: bool) -> bool {
    use std::io::Write;
    if yes {
        return true;
    }
    if !is_tty {
        println!("{prompt} — skipped (no tty — re-run with -y)");
        return false;
    }
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line);
    consented(&line)
}

/// The parse half of [`confirm`]: `y`/`yes` (case folded) consent; anything
/// else — including the empty string an EOF'd read leaves — declines. Split
/// out so answered-y, answered-n, and EOF-decline stay unit-tested (the tty
/// gate above keeps the real stdin out of reach in tests).
fn consented(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
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
    is_tty: bool,
    prompt: &str,
    pre_write: impl FnOnce() -> Result<(), String>,
) -> bool {
    if !confirm(prompt, yes, is_tty) {
        println!("{label}: skipped (declined)");
        return false;
    }
    if let Err(e) = pre_write() {
        crate::exit::fail_report(label, e);
        return false;
    }
    if let Err(e) = backup_then_write(path, new) {
        crate::exit::fail_report(label, format!("write failed — {e}"));
        return false;
    }
    true
}

/// Suffix of the pre-write backup `setup` leaves beside every file it edits.
/// The success epilogues interpolate it (via [`path_with_suffix`]) so the
/// advertised restore point can never drift from the file
/// [`backup_then_write`] actually creates.
pub(crate) const BACKUP_SUFFIX: &str = ".zj-radar.bak";

/// Back up the existing file, then write atomically (temp file + rename via the
/// shared `fsutil::atomic_write`). The `.bak` is specific to `setup` editing the
/// user's own files; `run` writes its owned dir without one. A failed backup
/// aborts the write: the success epilogues advertise the `.bak` as the restore
/// point, so the user's file must never be replaced without it existing.
pub(crate) fn backup_then_write(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if path.exists() {
        std::fs::copy(path, path_with_suffix(path, BACKUP_SUFFIX)).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("backup copy failed ({e}); {} left untouched", path.display()),
            )
        })?;
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

    #[test]
    fn consented_accepts_only_y_or_yes() {
        // Answered y (any case, either spelling) → consent.
        assert!(consented("y\n"));
        assert!(consented("Y\n"));
        assert!(consented("yes\n"));
        assert!(consented("  YES  \n"));
        // Answered n (or anything else) → decline.
        assert!(!consented("n\n"));
        assert!(!consented("no\n"));
        assert!(!consented("yep\n"));
        // EOF leaves the buffer empty — the default must be the safe "no".
        assert!(!consented(""));
    }

    #[test]
    fn confirm_yes_flag_consents_without_reading() {
        // `-y` wins before the tty gate: consent even with no tty to answer on.
        assert!(confirm("Write?", true, false));
        assert!(confirm("Write?", true, true));
        // No `-y`, no tty: the safe "no" (never blocks on a read).
        assert!(!confirm("Write?", false, false));
    }

    #[test]
    fn backup_then_write_aborts_when_backup_cannot_be_written() {
        // The success epilogues advertise the .bak as the restore point, so a
        // failed backup must abort the write, not overwrite-and-lie. Force the
        // copy to fail by occupying the .bak path with a directory.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.kdl");
        std::fs::write(&target, "original").unwrap();
        std::fs::create_dir(path_with_suffix(&target, BACKUP_SUFFIX)).unwrap();

        let err = backup_then_write(&target, "replacement").unwrap_err();
        assert!(err.to_string().contains("backup copy failed"), "err: {err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "original",
            "target must be untouched when the backup fails"
        );
    }
}
