//! `zj-radar setup copilot` — write a marker-owned hook file into Copilot
//! CLI's auto-loaded hooks directory (`$COPILOT_HOME/hooks/zj-radar.json`,
//! default `~/.copilot/hooks/zj-radar.json`). No settings.json edit, no
//! merge-in-place: Copilot CLI loads every `*.json` file it finds there, so
//! (unlike Codex's single shared `hooks.json`) this file is exclusively ours
//! and a clean uninstall is one file delete. Install, uninstall, `--check`
//! and producer detection all key on the header marker (`detect.rs`). The
//! install flow itself is pi's (`pi::setup_extension_file`) — the vendored
//! single-file model fits a JSON hook file as well as a JS bridge.

use super::detect::copilot_hook_is_ours;
use super::pi::{setup_extension_file, ExtensionFile};
use super::vendored::{read_existing, Existing};
use super::*;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The hook file: `$COPILOT_HOME/hooks/zj-radar.json`.
pub(crate) fn copilot_hooks_path() -> Option<PathBuf> {
    copilot_home_dir().map(|d| d.join("hooks").join(COPILOT_HOOK_FILE_NAME))
}

/// Read our hook file for producer detection — routed through the same
/// resolver `setup copilot` writes with, so `$COPILOT_HOME` is honored on
/// both sides.
pub(crate) fn copilot_hooks_text() -> Option<String> {
    copilot_hooks_path().and_then(|p| std::fs::read_to_string(p).ok())
}

/// Is our hook file on disk? The bare doctor's "copilot is set up here"
/// signal for a PATH-less copilot (an npm/Nix-run install).
pub(crate) fn copilot_hook_is_ours_on_disk() -> bool {
    copilot_hooks_text().is_some_and(|t| copilot_hook_is_ours(&t))
}

fn copilot_home_dir() -> Option<PathBuf> {
    copilot_home_from(std::env::var_os("COPILOT_HOME"), std::env::var_os("HOME"))
}

/// Resolve Copilot CLI's config home: `$COPILOT_HOME` wins, else
/// `$HOME/.copilot` (`copilot help environment`: "defaults to
/// `$HOME/.copilot`"). `None` when neither resolves — callers that *write*
/// refuse rather than invent a path, the same rule `codex_home_from` applies.
/// Pure (env passed in) so the precedence is unit-tested without touching
/// process env.
fn copilot_home_from(copilot_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(h) = copilot_home.filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(h));
    }
    home.filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".copilot"))
}

/// Pure: is Copilot CLI present? The binary on PATH, an existing
/// `$COPILOT_HOME`/`~/.copilot` dir (it holds config/state even before any
/// hook is installed), or our hook file already there.
fn copilot_installed_from(on_path: bool, home_dir_exists: bool, hook_exists: bool) -> bool {
    on_path || home_dir_exists || hook_exists
}

pub(crate) fn setup_copilot(uninstall: bool, opts: BridgeSetupOpts) {
    let (Some(home_dir), Some(path)) = (copilot_home_dir(), copilot_hooks_path()) else {
        crate::exit::fail_report(
            "copilot",
            "skipped — set $HOME or $COPILOT_HOME so Copilot's config dir can be resolved",
        );
        return;
    };
    let existing = read_existing(&path);
    if !uninstall && !copilot_installed_from(which("copilot"), home_dir.is_dir(), !matches!(existing, Existing::Absent))
    {
        return report_skip("copilot", "binary/config dir not found", opts.explicit);
    }
    setup_extension_file(
        // `js` just means "the static content we write" here — Copilot's hook
        // file is JSON, not JavaScript; the field is shared with the
        // opencode/pi/omp bridges, which are JS.
        &ExtensionFile {
            label: "copilot",
            path: Path::new(&path),
            js: COPILOT_HOOKS_JSON,
            is_ours: copilot_hook_is_ours,
            restart_hint: "start a new `copilot` session",
        },
        &existing,
        uninstall,
        opts,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn copilot_home_prefers_copilot_home_over_home() {
        assert_eq!(copilot_home_from(Some(os("/x/copilot")), Some(os("/home/u"))), Some(PathBuf::from("/x/copilot")));
    }

    #[test]
    fn copilot_home_falls_back_to_home_dot_copilot() {
        assert_eq!(copilot_home_from(None, Some(os("/home/u"))), Some(PathBuf::from("/home/u/.copilot")));
    }

    #[test]
    fn copilot_home_is_none_when_neither_resolves() {
        assert_eq!(copilot_home_from(None, None), None);
        assert_eq!(copilot_home_from(Some(OsString::new()), Some(OsString::new())), None);
        assert_eq!(copilot_home_from(None, Some(OsString::new())), None);
        assert_eq!(
            copilot_home_from(Some(OsString::new()), Some(os("/home/u"))),
            Some(PathBuf::from("/home/u/.copilot"))
        );
    }

    #[test]
    fn copilot_counts_as_installed_via_binary_home_dir_or_hook_file() {
        assert!(copilot_installed_from(true, false, false));
        assert!(copilot_installed_from(false, true, false));
        assert!(copilot_installed_from(false, false, true));
        assert!(!copilot_installed_from(false, false, false));
    }

    /// Weld: the embedded hook file is valid JSON, carries the marker the
    /// install path, doctor and producer detection key on, and wires every
    /// event `agents/copilot.rs`'s `every_wired_status_token_derives_an_update`
    /// test expects a status for.
    #[test]
    fn embedded_hooks_json_is_valid_and_carries_marker_and_contract() {
        let parsed: serde_json::Value =
            serde_json::from_str(COPILOT_HOOKS_JSON).expect("COPILOT_HOOKS_JSON must be valid JSON");
        assert_eq!(parsed["version"], 1);
        assert!(COPILOT_HOOK_MARKER.starts_with(COPILOT_HOOK_MARKER_PREFIX));
        assert!(COPILOT_HOOKS_JSON.contains(COPILOT_HOOK_MARKER));
        let wired_events = ["sessionStart", "sessionEnd", "userPromptSubmitted", "agentStop", "errorOccurred"];
        for event in wired_events {
            let hooks = parsed["hooks"][event].as_array().unwrap_or_else(|| panic!("missing {event} hook entry"));
            assert_eq!(hooks.len(), 1, "{event} must wire exactly one zj-radar entry");
            let entry = &hooks[0];
            assert_eq!(entry["type"], "command");
            let command = entry["command"].as_str().unwrap();
            assert!(command.starts_with("zj-radar notify copilot --status "), "{event}: {command}");
            assert_eq!(entry["env"]["ZJ_RADAR_COPILOT_HOOK"], "v1", "{event} must carry the ownership marker");
            // No tool-decision event is wired, so no entry needs a matcher.
            assert!(entry.get("matcher").is_none(), "{event} has no tool to match");
        }
        assert_eq!(parsed["hooks"].as_object().unwrap().len(), wired_events.len(), "an unreviewed event got wired");
        // Real-CLI finding (Copilot 1.0.117): these deny the tool call when
        // their hook command fails, so they must never be wired — pinned
        // here so a future edit can't silently reintroduce them.
        for unsafe_event in ["preToolUse", "permissionRequest", "postToolUse", "subagentStart", "subagentStop"] {
            assert!(parsed["hooks"].get(unsafe_event).is_none(), "{unsafe_event} must never be wired");
        }
    }
}
