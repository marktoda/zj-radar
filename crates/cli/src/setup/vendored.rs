//! One vendored bridge file's install/uninstall decision — shared by every
//! producer whose wiring is "drop a marked file into the agent's auto-loaded
//! dir" (opencode, pi). Pure; the caller supplies the ownership test.

/// What `setup <agent>` found at a bridge path. Three-state on purpose:
/// `Absent` must never collapse into `Text("")` — the foreign-file refusal
/// gate and the `--uninstall` report both key on the difference — and a file
/// that is present but unreadable (non-UTF8, permissions, a directory where
/// a file should be) is not ours to overwrite either.
pub(crate) enum Existing {
    Absent,
    Text(String),
    Unreadable(std::io::Error),
}

pub(crate) fn read_existing(path: &std::path::Path) -> Existing {
    match std::fs::read_to_string(path) {
        Ok(text) => Existing::Text(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Existing::Absent,
        Err(e) => Existing::Unreadable(e),
    }
}

impl Existing {
    /// The bridge text for the agent's `*Env`, honoring its `None` = absent
    /// contract (an unreadable file has no text to classify, so it is `None`
    /// too).
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Existing::Text(t) => Some(t),
            Existing::Absent | Existing::Unreadable(_) => None,
        }
    }

    /// Why `path` (holding `self`) is refused under [`InstallPlan::RefuseForeign`]
    /// — the one wording every vendored-bridge target reports.
    pub(crate) fn refusal_reason(&self, path: &std::path::Path) -> String {
        match self {
            Existing::Unreadable(e) => format!("{} could not be read ({e})", path.display()),
            Existing::Absent | Existing::Text(_) => format!("{} is not ours (no marker)", path.display()),
        }
    }
}

/// After an uninstall removed the bridge at `path`, remove its `.bak` restore
/// point too — but only when that backup carries our marker (a stale-ours
/// rewrite). After `--force` over a foreign file the backup IS the user's
/// only copy of it, so it stays. Returns the backup's path when one was left
/// behind, for the caller to mention.
pub(crate) fn remove_backup_if_ours(path: &std::path::Path, is_ours: fn(&str) -> bool) -> Option<std::path::PathBuf> {
    let bak = super::path_with_suffix(path, super::BACKUP_SUFFIX);
    match plan_uninstall(&read_existing(&bak), is_ours) {
        UninstallPlan::Absent => None,
        UninstallPlan::Remove => {
            let _ = std::fs::remove_file(&bak);
            None
        }
        UninstallPlan::NotOurs => Some(bak),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InstallPlan {
    /// Ours and byte-identical to the embedded bridge.
    UpToDate,
    /// A file we can't claim (no marker, or unreadable) and no `--force`.
    RefuseForeign,
    /// Absent, empty, stale-ours, or `--force` over a foreign file.
    Write,
}

/// Pure: the install decision for one bridge file. The embedded JS is the
/// single source of truth, so "already up to date" is a byte-identical
/// compare; a foreign file (no marker) is refused unless `force`, mirroring
/// the codex notify-slot rule. `is_ours` is the caller's marker test
/// (`opencode_plugin_is_ours`, `pi_extension_is_ours`, …).
pub(crate) fn plan_install(existing: &Existing, embedded: &str, force: bool, is_ours: fn(&str) -> bool) -> InstallPlan {
    match existing {
        Existing::Absent => InstallPlan::Write,
        Existing::Unreadable(_) if force => InstallPlan::Write,
        Existing::Unreadable(_) => InstallPlan::RefuseForeign,
        Existing::Text(text) => {
            let ours = is_ours(text);
            if ours && text == embedded {
                InstallPlan::UpToDate
            } else if !text.is_empty() && !ours && !force {
                InstallPlan::RefuseForeign
            } else {
                InstallPlan::Write
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UninstallPlan {
    /// Nothing at the path — nothing to do, and not an error.
    Absent,
    /// A file is there but carries no marker (or can't be read): leave it.
    NotOurs,
    Remove,
}

/// Pure: the uninstall decision — only a marker-bearing file is ours to delete.
pub(crate) fn plan_uninstall(existing: &Existing, is_ours: fn(&str) -> bool) -> UninstallPlan {
    match existing {
        Existing::Absent => UninstallPlan::Absent,
        Existing::Text(text) if is_ours(text) => UninstallPlan::Remove,
        Existing::Text(_) | Existing::Unreadable(_) => UninstallPlan::NotOurs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::detect::opencode_plugin_is_ours;
    use super::super::{OPENCODE_PLUGIN_JS, OPENCODE_TUI_PLUGIN_JS};

    fn unreadable() -> Existing {
        Existing::Unreadable(std::io::Error::new(std::io::ErrorKind::InvalidData, "stream did not contain valid UTF-8"))
    }

    #[test]
    fn plan_install_writes_when_absent_or_empty() {
        assert_eq!(plan_install(&Existing::Absent, OPENCODE_PLUGIN_JS, false, opencode_plugin_is_ours), InstallPlan::Write);
        // An empty file is nobody's plugin — write over it without --force.
        assert_eq!(
            plan_install(&Existing::Text(String::new()), OPENCODE_PLUGIN_JS, false, opencode_plugin_is_ours),
            InstallPlan::Write
        );
    }

    #[test]
    fn plan_install_is_up_to_date_only_when_byte_identical() {
        assert_eq!(
            plan_install(&Existing::Text(OPENCODE_PLUGIN_JS.to_string()), OPENCODE_PLUGIN_JS, false, opencode_plugin_is_ours),
            InstallPlan::UpToDate
        );
        assert_eq!(
            plan_install(&Existing::Text(OPENCODE_TUI_PLUGIN_JS.to_string()), OPENCODE_TUI_PLUGIN_JS, false, opencode_plugin_is_ours),
            InstallPlan::UpToDate
        );
        // Ours (marker present) but stale → rewrite, no --force needed.
        let stale = format!("// {}\n// older bridge\n", super::super::OPENCODE_PLUGIN_MARKER);
        assert_eq!(plan_install(&Existing::Text(stale), OPENCODE_PLUGIN_JS, false, opencode_plugin_is_ours), InstallPlan::Write);
        // The two bridges are never interchangeable: the 1.x text at the 2.x
        // path is ours, but stale.
        assert_eq!(
            plan_install(&Existing::Text(OPENCODE_PLUGIN_JS.to_string()), OPENCODE_TUI_PLUGIN_JS, false, opencode_plugin_is_ours),
            InstallPlan::Write
        );
    }

    #[test]
    fn plan_install_refuses_foreign_plugin_without_force() {
        let foreign = Existing::Text("export const Other = async () => ({});\n".to_string());
        assert_eq!(plan_install(&foreign, OPENCODE_PLUGIN_JS, false, opencode_plugin_is_ours), InstallPlan::RefuseForeign);
        assert_eq!(plan_install(&foreign, OPENCODE_PLUGIN_JS, true, opencode_plugin_is_ours), InstallPlan::Write);
    }

    #[test]
    fn plan_install_treats_unreadable_file_as_foreign() {
        // A present-but-unreadable file (non-UTF8, permissions) must NOT read as
        // absent — that would skip the refusal gate and overwrite it silently.
        assert_eq!(plan_install(&unreadable(), OPENCODE_PLUGIN_JS, false, opencode_plugin_is_ours), InstallPlan::RefuseForeign);
        assert_eq!(plan_install(&unreadable(), OPENCODE_PLUGIN_JS, true, opencode_plugin_is_ours), InstallPlan::Write);
    }

    #[test]
    fn remove_backup_if_ours_keeps_a_foreign_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zj-radar.js");
        let bak = super::super::path_with_suffix(&path, super::super::BACKUP_SUFFIX);

        assert_eq!(remove_backup_if_ours(&path, opencode_plugin_is_ours), None, "no backup: nothing to do");

        std::fs::write(&bak, OPENCODE_PLUGIN_JS).unwrap();
        assert_eq!(remove_backup_if_ours(&path, opencode_plugin_is_ours), None);
        assert!(!bak.exists(), "a marker-bearing backup is ours to remove");

        // `--force` over a foreign file: the backup is the user's only copy.
        std::fs::write(&bak, "export const Theirs = async () => ({});\n").unwrap();
        assert_eq!(remove_backup_if_ours(&path, opencode_plugin_is_ours), Some(bak.clone()));
        assert!(bak.exists(), "a foreign backup must survive uninstall");
    }

    #[test]
    fn refusal_reason_names_unreadable_and_foreign_files() {
        let p = std::path::Path::new("/x/zj-radar.js");
        assert_eq!(Existing::Text("x".into()).refusal_reason(p), "/x/zj-radar.js is not ours (no marker)");
        assert!(unreadable().refusal_reason(p).starts_with("/x/zj-radar.js could not be read ("));
    }

    #[test]
    fn plan_uninstall_distinguishes_absent_from_foreign() {
        assert_eq!(plan_uninstall(&Existing::Absent, opencode_plugin_is_ours), UninstallPlan::Absent);
        assert_eq!(plan_uninstall(&Existing::Text("// not ours\n".to_string()), opencode_plugin_is_ours), UninstallPlan::NotOurs);
        assert_eq!(plan_uninstall(&unreadable(), opencode_plugin_is_ours), UninstallPlan::NotOurs);
        assert_eq!(plan_uninstall(&Existing::Text(OPENCODE_PLUGIN_JS.to_string()), opencode_plugin_is_ours), UninstallPlan::Remove);
        assert_eq!(plan_uninstall(&Existing::Text(OPENCODE_TUI_PLUGIN_JS.to_string()), opencode_plugin_is_ours), UninstallPlan::Remove);
    }
}
