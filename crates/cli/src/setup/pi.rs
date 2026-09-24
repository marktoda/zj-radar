//! `zj-radar setup pi` — vendor the bridge extension into pi's auto-loaded
//! global extensions dir (`<agent dir>/extensions/zj-radar.js`). No settings
//! edit; a clean uninstall is one file delete. Install, uninstall, `--check`
//! and producer detection all key on the header marker (`detect.rs`).

use super::*;
// Task 5 adds, when it appends setup_pi below:
// use super::detect::pi_extension_is_ours;
// use super::vendored::{plan_install, plan_uninstall, read_existing, Existing, InstallPlan, UninstallPlan};

use std::ffi::OsString;
use std::path::PathBuf;

/// The bridge file: `<agent dir>/extensions/zj-radar.js`.
pub(crate) fn pi_extension_path() -> Option<PathBuf> {
    pi_agent_dir().map(|d| d.join("extensions").join(PI_EXTENSION_FILE_NAME))
}

/// Read the vendored bridge for producer detection — routed through the same
/// resolver `setup pi` writes with, so `$PI_CODING_AGENT_DIR` is honored on
/// both sides.
pub(crate) fn pi_extension_text() -> Option<String> {
    pi_extension_path().and_then(|p| std::fs::read_to_string(p).ok())
}

fn pi_agent_dir() -> Option<PathBuf> {
    pi_agent_dir_from(std::env::var_os("PI_CODING_AGENT_DIR"), std::env::var_os("HOME"))
}

/// Resolve pi's agent dir the way pi's `getAgentDir()` does
/// (`dist/config.js`): `$PI_CODING_AGENT_DIR` wins (with a leading `~`
/// expanded against `$HOME`), else `$HOME/.pi/agent`. `None` when neither
/// resolves. Pure (env passed in) so the precedence is unit-tested.
fn pi_agent_dir_from(override_dir: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let home = home.filter(|h| !h.is_empty()).map(PathBuf::from);
    if let Some(dir) = override_dir.filter(|d| !d.is_empty()) {
        let text = dir.to_string_lossy();
        if text == "~" {
            return home;
        }
        if let Some(rest) = text.strip_prefix("~/") {
            return home.map(|h| h.join(rest));
        }
        return Some(PathBuf::from(dir));
    }
    home.map(|h| h.join(".pi").join("agent"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn pi_agent_dir_prefers_the_override_and_expands_tilde() {
        assert_eq!(pi_agent_dir_from(Some(os("/x/pi")), Some(os("/home/u"))), Some(PathBuf::from("/x/pi")));
        assert_eq!(pi_agent_dir_from(Some(os("~/p")), Some(os("/home/u"))), Some(PathBuf::from("/home/u/p")));
        assert_eq!(pi_agent_dir_from(Some(os("~")), Some(os("/home/u"))), Some(PathBuf::from("/home/u")));
        assert_eq!(pi_agent_dir_from(Some(os("~/p")), None), None, "a tilde override needs a HOME");
    }

    #[test]
    fn pi_agent_dir_falls_back_to_home_and_treats_empty_as_unset() {
        assert_eq!(pi_agent_dir_from(None, Some(os("/home/u"))), Some(PathBuf::from("/home/u/.pi/agent")));
        assert_eq!(pi_agent_dir_from(Some(OsString::new()), Some(os("/home/u"))), Some(PathBuf::from("/home/u/.pi/agent")));
        assert_eq!(pi_agent_dir_from(None, Some(OsString::new())), None);
        assert_eq!(pi_agent_dir_from(None, None), None);
    }
}
